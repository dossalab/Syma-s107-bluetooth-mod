use defmt::{debug, error, unwrap, warn};
use embassy_futures::select::{select, select3, Either, Either3};
use embassy_time::Timer;
use nrf_softdevice::ble::advertisement_builder::{
    Flag, LegacyAdvertisementBuilder, LegacyAdvertisementPayload, ServiceList,
};
use nrf_softdevice::ble::{gatt_server, peripheral, Connection, Primitive};
use nrf_softdevice::Softdevice;

use crate::state::{Request, SystemState};
use crate::types::{ChargerState, PeriodicUpdate, PidParams};

use super::errors::BleError;

#[nrf_softdevice::gatt_service(uuid = "180f")]
pub struct BatteryService {
    #[characteristic(uuid = "2a19", read, notify)]
    battery_level: u8,
}

unsafe impl Primitive for PeriodicUpdate {}
unsafe impl Primitive for ChargerState {}
unsafe impl Primitive for PidParams {}

// Help clients find us by using that uuid
const POWER_SERVICE_UUID_BYTES: [u8; 16] =
    0x38924a07_23d7_43fe_af5d_9c887a089cf1_u128.to_le_bytes();

// bas is too limited to share everything we have
#[nrf_softdevice::gatt_service(uuid = "38924a07-23d7-43fe-af5d-9c887a089cf1")]
pub struct PowerService {
    #[characteristic(uuid = "38924a07-23d7-43fe-af5d-9c887a189cf1", read, notify)]
    charger_state: ChargerState,

    #[characteristic(uuid = "38924a07-23d7-43fe-af5d-9c887a289cf1", notify)]
    periodic_update: PeriodicUpdate,

    #[characteristic(uuid = "38924a07-23d7-43fe-af5d-9c887a389cf1", notify)]
    gyro: i16,
}

#[nrf_softdevice::gatt_service(uuid = "38924a07-23d7-43fe-af5d-9c887b089cf1")]
pub struct RequestsService {
    #[characteristic(uuid = "38924a07-23d7-43fe-af5d-9c887b189cf1", write)]
    reboot: bool,

    #[characteristic(uuid = "38924a07-23d7-43fe-af5d-9c887b289cf1", write)]
    pid_update: PidParams,

    #[characteristic(uuid = "38924a07-23d7-43fe-af5d-9c887b389cf1", write)]
    fuelgauge_reset: bool,
}

#[nrf_softdevice::gatt_server]
pub struct GattServer {
    bas: BatteryService,
    power: PowerService,
    requests: RequestsService,
}

async fn run_gatt(server: &GattServer, conn: &Connection, state: &SystemState) {
    let host_request_sender = state.requests.sender();

    let handle_bas = |e| match e {
        _ => {}
    };

    let handle_requests = |e| {
        let request = match e {
            RequestsServiceEvent::RebootWrite(true) => Request::Reboot,
            RequestsServiceEvent::PidUpdateWrite(pid) => Request::PidUpdate(pid),
            RequestsServiceEvent::FuelgaugeResetWrite(true) => Request::FuelgaugeReset,

            _ => return,
        };

        host_request_sender.send(request);
    };

    let handle_power = |e| match e {
        _ => {}
    };

    gatt_server::run(conn, server, |e| match e {
        GattServerEvent::Bas(e) => handle_bas(e),
        GattServerEvent::Requests(e) => handle_requests(e),
        GattServerEvent::Power(e) => handle_power(e),
    })
    .await;
}

async fn run_notifications(
    state: &SystemState,
    conn: &Connection,
    server: &GattServer,
) -> Result<(), BleError> {
    let mut soc_receiver = unwrap!(state.soc.receiver());
    let mut charger_state_receiver = unwrap!(state.charger_state.receiver());
    let mut periodic_update_receiver = unwrap!(state.periodic_update.receiver());

    if let Some(soc) = soc_receiver.try_get() {
        server.bas.battery_level_set(&soc)?;
    }

    if let Some(charger_state) = charger_state_receiver.try_get() {
        server.power.charger_state_set(&charger_state)?;
    }

    loop {
        let r = select3(
            soc_receiver.changed(),
            charger_state_receiver.changed(),
            periodic_update_receiver.changed(),
        )
        .await;

        let err = match r {
            Either3::First(x) => server.bas.battery_level_notify(conn, &x),
            Either3::Second(x) => server.power.charger_state_notify(conn, &x),
            Either3::Third(x) => server.power.periodic_update_notify(conn, &x),
        };

        if let Err(x) = err {
            warn!("unable to notify - {}", x);
        }
    }
}

pub async fn peripheral_loop(sd: &Softdevice, ps: &'static SystemState, server: &GattServer) {
    static ADV_DATA: LegacyAdvertisementPayload = LegacyAdvertisementBuilder::new()
        .flags(&[Flag::GeneralDiscovery, Flag::LE_Only])
        .services_128(ServiceList::Incomplete, &[POWER_SERVICE_UUID_BYTES])
        .build();

    static SCAN_DATA: LegacyAdvertisementPayload = LegacyAdvertisementBuilder::new()
        .full_name("Syma S107")
        .build();

    let config = peripheral::Config {
        interval: 1600, // * 0.625us
        ..peripheral::Config::default()
    };

    let adv = peripheral::ConnectableAdvertisement::ScannableUndirected {
        adv_data: &ADV_DATA,
        scan_data: &SCAN_DATA,
    };

    loop {
        match peripheral::advertise_connectable(sd, adv, &config).await {
            Ok(conn) => {
                let r = select(
                    run_gatt(&server, &conn, ps),
                    run_notifications(ps, &conn, &server),
                )
                .await;

                match r {
                    Either::First(_) => debug!("gatt finished"),
                    Either::Second(r) => {
                        debug!("notification dispatcher finished");
                        if let Err(e) = r {
                            error!("notification dispatcher error - {}", e);
                        }
                    }
                }
            }

            Err(e) => {
                error!("unable to advertise - {}", e);

                // might need some time to recover
                Timer::after_secs(1).await;
            }
        }
    }
}
