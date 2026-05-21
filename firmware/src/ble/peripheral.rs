use crate::ble::types::{
    GattServer, GattServerEvent, RequestsServiceEvent, TELEMETRY_SERVICE_UUID,
};
use defmt::{error, info, unwrap};
use embassy_futures::select::{select, Either};
use embassy_time::{Duration, Instant, Ticker, Timer};
use nrf_softdevice::ble::advertisement_builder::{
    Flag, LegacyAdvertisementBuilder, LegacyAdvertisementPayload, ServiceList,
};
use nrf_softdevice::ble::{gatt_server, peripheral, Connection};
use nrf_softdevice::Softdevice;

use super::errors::BleError;
use crate::state::{StateReceiver, SystemState};
use crate::types::Request;

// Help clients find us by using that uuid
const TELEMETRY_SERVICE_UUID_BYTES: [u8; 16] = TELEMETRY_SERVICE_UUID.to_le_bytes();

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
            RequestsServiceEvent::ControlUpdateWrite(params) => Request::ControlUpdate(params),
            RequestsServiceEvent::FuelgaugeResetWrite(true) => Request::FuelgaugeReset,
            RequestsServiceEvent::ScanStateWrite(true) => Request::StartScan,

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

async fn notify_watch<T: Clone>(mut recv: StateReceiver<'_, T>, mut on_change: impl FnMut(&T)) {
    if let Some(v) = recv.try_get() {
        on_change(&v);
    }
    loop {
        on_change(&recv.changed().await);
    }
}

async fn run_notifications(state: &SystemState, conn: &Connection, server: &GattServer) -> ! {
    futures::join!(
        notify_watch(unwrap!(state.soc.receiver()), |x| {
            server.bas.battery_level_set(x).ok();
            server.bas.battery_level_notify(conn, x).ok();
        }),
        notify_watch(unwrap!(state.charger_state.receiver()), |x| {
            server.telemetry.charger_state_set(x).ok();
            server.telemetry.charger_state_notify(conn, x).ok();
        }),
        notify_watch(unwrap!(state.periodic_update.receiver()), |x| {
            server.telemetry.periodic_update_notify(conn, x).ok();
        }),
        notify_watch(unwrap!(state.qmax_update.receiver()), |x| {
            server.telemetry.qmax_update_set(x).ok();
            server.telemetry.qmax_update_notify(conn, x).ok();
        }),
        notify_watch(unwrap!(state.ocv_measurement.receiver()), |x| {
            server.telemetry.ocv_measurement_set(x).ok();
            server.telemetry.ocv_measurement_notify(conn, x).ok();
        }),
        notify_watch(unwrap!(state.ratable_update.receiver()), |x| {
            server.telemetry.ratable_update_set(x).ok();
            server.telemetry.ratable_update_notify(conn, x).ok();
        }),
        notify_watch(unwrap!(state.scan_state.receiver()), |x| {
            server.requests.scan_state_set(x).ok();
            server.requests.scan_state_notify(conn, x).ok();
        }),
        notify_watch(unwrap!(state.controller_connected.receiver()), |x| {
            server.telemetry.controller_connected_set(x).ok();
            server.telemetry.controller_connected_notify(conn, x).ok();
        }),
    );

    unreachable!()
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
                if let Err(e) = gatt_server::set_sys_attrs(&conn, None) {
                    error!("set_sys_attrs failed - {}", e);
                    continue;
                }

                let Either::First(r) = select(
                    run_gatt(&server, &conn, ps),
                    run_notifications(ps, &conn, &server),
                )
                .await;

                info!("gatt finished");

                if let Err(e) = r {
                    error!("gatt error - {}", e);
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
