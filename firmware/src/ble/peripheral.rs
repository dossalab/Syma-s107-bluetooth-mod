use defmt::{debug, error, info, unwrap, warn};
use embassy_futures::select::{select, Either};
use embassy_time::Timer;
use nrf_softdevice::ble::advertisement_builder::{
    Flag, LegacyAdvertisementBuilder, LegacyAdvertisementPayload, ServiceList, ServiceUuid16,
};
use nrf_softdevice::ble::{gatt_server, peripheral, Connection, Primitive};
use nrf_softdevice::Softdevice;

use crate::power::stats::{PeriodicUpdate, PowerStats, UpdateType};

use super::errors::BleError;

#[nrf_softdevice::gatt_service(uuid = "180f")]
pub struct BatteryService {
    #[characteristic(uuid = "2a19", read, notify)]
    battery_level: u8,
}

unsafe impl Primitive for PeriodicUpdate {}

// bas is too limited to share everything we have
#[nrf_softdevice::gatt_service(uuid = "38924a07-23d7-43fe-af5d-9c887a089cf1")]
pub struct PowerService {
    #[characteristic(uuid = "38924a07-23d7-43fe-af5d-9c887a189cf1", read, notify)]
    charger_connected: bool,

    #[characteristic(uuid = "38924a07-23d7-43fe-af5d-9c887a289cf1", read, notify)]
    charger_failure: bool,

    #[characteristic(uuid = "38924a07-23d7-43fe-af5d-9c887a389cf1", read, notify)]
    periodic_update: PeriodicUpdate,
}

#[nrf_softdevice::gatt_service(uuid = "38924a07-23d7-43fe-af5d-9c887b089cf1")]
pub struct ControlService {
    #[characteristic(uuid = "38924a07-23d7-43fe-af5d-9c887b189cf1", write)]
    reboot_request: bool,
}

#[nrf_softdevice::gatt_server]
pub struct GattServer {
    bas: BatteryService,
    power: PowerService,
    control: ControlService,
}

async fn run_gatt(server: &GattServer, conn: &Connection) {
    let handle_bas = |e| match e {
        BatteryServiceEvent::BatteryLevelCccdWrite { notifications } => {
            info!("battery notifications: {}", notifications)
        }
    };

    let handle_control = |e| match e {
        ControlServiceEvent::RebootRequestWrite(_) => {}
    };

    gatt_server::run(conn, server, |e| match e {
        GattServerEvent::Bas(e) => handle_bas(e),
        GattServerEvent::Control(e) => handle_control(e),
        _ => {}
    })
    .await;
}

async fn run_notifications(
    ps: &PowerStats,
    conn: &Connection,
    server: &GattServer,
) -> Result<(), BleError> {
    // Sync current state once
    server.bas.battery_level_set(&ps.soc())?;
    server.power.charger_connected_set(&ps.is_charging())?;
    server
        .power
        .charger_failure_set(&ps.is_charging_failure())?;
    server.power.periodic_update_set(&ps.periodic_snapshot())?;

    // And then wait for the updates
    let mut receiver = unwrap!(ps.event_receiver());
    loop {
        let mut next_update = async || -> Result<(), BleError> {
            match receiver.changed().await {
                UpdateType::Soc(v) => server.bas.battery_level_notify(conn, &v)?,
                UpdateType::ChargingStatus(v) => server.power.charger_connected_notify(conn, &v)?,
                UpdateType::ChargingFailure(v) => server.power.charger_failure_notify(conn, &v)?,
                UpdateType::PeriodicUpdate(v) => server.power.periodic_update_notify(conn, &v)?,
            }

            Ok(())
        };

        // XXX: we fail to notify if corresponding notification is disabled - is this a problem?
        if let Err(e) = next_update().await {
            warn!("notify error - {}", e)
        }
    }
}

pub async fn peripheral_loop(sd: &Softdevice, ps: &'static PowerStats, server: GattServer) {
    static ADV_DATA: LegacyAdvertisementPayload = LegacyAdvertisementBuilder::new()
        .flags(&[Flag::GeneralDiscovery, Flag::LE_Only])
        .services_16(ServiceList::Complete, &[ServiceUuid16::HEALTH_THERMOMETER])
        .short_name("Hello")
        .build();

    static SCAN_DATA: LegacyAdvertisementPayload = LegacyAdvertisementBuilder::new()
        .full_name("Hello, Rust!")
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
                    run_gatt(&server, &conn),
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
