use defmt::{error, info, unwrap, warn};
use embassy_futures::select::{select, select5, Either, Either5};
use embassy_time::{Duration, Instant, Ticker, Timer};
use nrf_softdevice::ble::advertisement_builder::{
    Flag, LegacyAdvertisementBuilder, LegacyAdvertisementPayload, ServiceList,
};
use nrf_softdevice::ble::{gatt_server, peripheral, Connection, Primitive};
use nrf_softdevice::Softdevice;

use crate::state::SystemState;
use crate::types::{
    ChargerState, OcvMeasurement, PeriodicUpdate, PidParams, QmaxUpdate, Request, SecInstant,
};

use super::errors::BleError;

#[nrf_softdevice::gatt_service(uuid = "180f")]
pub struct BatteryService {
    #[characteristic(uuid = "2a19", read, notify)]
    battery_level: u8,
}

unsafe impl Primitive for PeriodicUpdate {}
unsafe impl Primitive for ChargerState {}
unsafe impl Primitive for PidParams {}
unsafe impl Primitive for QmaxUpdate {}
unsafe impl Primitive for OcvMeasurement {}
unsafe impl Primitive for SecInstant {}

// Help clients find us by using that uuid
const TELEMETRY_SERVICE_UUID_BYTES: [u8; 16] =
    0x38924a07_23d7_43fe_af5d_9c887a089cf1_u128.to_le_bytes();

// bas is too limited to share everything we have
#[nrf_softdevice::gatt_service(uuid = "38924a07-23d7-43fe-af5d-9c887a089cf1")]
pub struct TelemetryService {
    #[characteristic(uuid = "38924a07-23d7-43fe-af5d-9c887a189cf1", read, notify)]
    charger_state: ChargerState,

    #[characteristic(uuid = "38924a07-23d7-43fe-af5d-9c887a289cf1", notify)]
    periodic_update: PeriodicUpdate,

    #[characteristic(uuid = "38924a07-23d7-43fe-af5d-9c887a389cf1", read, notify)]
    qmax_update: QmaxUpdate,

    #[characteristic(uuid = "38924a07-23d7-43fe-af5d-9c887a489cf1", read, notify)]
    ocv_measurement: OcvMeasurement,

    #[characteristic(uuid = "38924a07-23d7-43fe-af5d-9c887a589cf1", read, notify)]
    time: SecInstant,
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
    telemetry: TelemetryService,
    requests: RequestsService,
}

async fn run_gatt(
    server: &GattServer,
    conn: &Connection,
    state: &SystemState,
) -> Result<(), BleError> {
    let host_request_sender = state.requests.sender();
    let mut ticker = Ticker::every(Duration::from_secs(1));

    let handle_requests = |e| {
        let request = match e {
            RequestsServiceEvent::RebootWrite(true) => Request::Reboot,
            RequestsServiceEvent::PidUpdateWrite(pid) => Request::PidUpdate(pid),
            RequestsServiceEvent::FuelgaugeResetWrite(true) => Request::FuelgaugeReset,

            _ => return,
        };

        host_request_sender.send(request);
    };

    let callback = |event| match event {
        GattServerEvent::Bas(_e) => {}
        GattServerEvent::Requests(e) => handle_requests(e),
        GattServerEvent::Telemetry(_e) => {}
    };

    let mut run_clock = async || -> Result<(), BleError> {
        loop {
            let time = Instant::now();

            server.telemetry.time_set(&time.into())?;
            _ = server.telemetry.time_notify(&conn, &time.into());
            ticker.next().await;
        }
    };

    let r = select(gatt_server::run(conn, server, callback), run_clock()).await;
    match r {
        Either::First(_) => Ok(()), // disconnected is not an error
        Either::Second(e) => e,
    }
}

async fn run_notifications(
    state: &SystemState,
    conn: &Connection,
    server: &GattServer,
) -> Result<(), BleError> {
    let mut soc_receiver = unwrap!(state.soc.receiver());
    let mut charger_state_receiver = unwrap!(state.charger_state.receiver());
    let mut periodic_update_receiver = unwrap!(state.periodic_update.receiver());
    let mut qmax_update_receiver = unwrap!(state.qmax_update.receiver());
    let mut ocv_measurement_receiver = unwrap!(state.ocv_measurement.receiver());

    if let Some(soc) = soc_receiver.try_get() {
        server.bas.battery_level_set(&soc)?;
    }

    if let Some(charger_state) = charger_state_receiver.try_get() {
        server.telemetry.charger_state_set(&charger_state)?;
    }

    if let Some(qmax_update) = qmax_update_receiver.try_get() {
        server.telemetry.qmax_update_set(&qmax_update)?;
    }

    if let Some(ocv_measurement) = ocv_measurement_receiver.try_get() {
        server.telemetry.ocv_measurement_set(&ocv_measurement)?;
    }

    loop {
        let r = select5(
            soc_receiver.changed(),
            charger_state_receiver.changed(),
            periodic_update_receiver.changed(),
            qmax_update_receiver.changed(),
            ocv_measurement_receiver.changed(),
        )
        .await;

        let err = match r {
            Either5::First(x) => server.bas.battery_level_notify(conn, &x),
            Either5::Second(x) => server.telemetry.charger_state_notify(conn, &x),
            Either5::Third(x) => server.telemetry.periodic_update_notify(conn, &x),
            Either5::Fourth(x) => server.telemetry.qmax_update_notify(conn, &x),
            Either5::Fifth(x) => server.telemetry.ocv_measurement_notify(conn, &x),
        };

        if let Err(x) = err {
            warn!("unable to notify - {}", x);
        }
    }
}

pub async fn peripheral_loop(sd: &Softdevice, ps: &'static SystemState, server: &GattServer) {
    static ADV_DATA: LegacyAdvertisementPayload = LegacyAdvertisementBuilder::new()
        .flags(&[Flag::GeneralDiscovery, Flag::LE_Only])
        .services_128(ServiceList::Incomplete, &[TELEMETRY_SERVICE_UUID_BYTES])
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
                    Either::First(r) => {
                        info!("gatt finished");
                        if let Err(e) = r {
                            error!("gatt error - {}", e);
                        }
                    }
                    Either::Second(r) => {
                        info!("notification dispatcher finished");
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
