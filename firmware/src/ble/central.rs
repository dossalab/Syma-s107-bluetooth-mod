use defmt::{debug, error, info, warn};
use embassy_futures::select::{select, Either};
use embassy_time::{Duration, Timer};
use nrf_softdevice::{
    ble::{
        self, central, gatt_client, security::SecurityHandler, Address, AddressType, EncryptError,
        EncryptionInfo,
    },
    Softdevice,
};
use scopeguard::guard;

use crate::state::SystemState;
use crate::xbox::XboxHidServiceClient;
use crate::{
    indications::{IndicationStyle, LedIndicationsSignal},
    xbox::{self, XboxHidServiceClientEvent},
};

use super::errors::BleError;

pub struct Bonder {}

impl Default for Bonder {
    fn default() -> Self {
        Bonder {}
    }
}

impl SecurityHandler for Bonder {
    fn can_bond(&self, _conn: &nrf_softdevice::ble::Connection) -> bool {
        true
    }

    fn on_bonded(
        &self,
        _conn: &ble::Connection,
        _master_id: ble::MasterId,
        _key: EncryptionInfo,
        _peer_id: ble::IdentityKey,
    ) {
        info!("on_bonded is called!")
    }
}

// Scan for Xbox controllers
async fn scan(sd: &Softdevice, indications: &'static LedIndicationsSignal) -> Option<Address> {
    let config = central::ScanConfig {
        interval: 3200, // *0.625 us
        window: 160,    // *0.625us
        ..central::ScanConfig::default()
    };

    let timeout = Duration::from_secs(10);

    let do_scan = async || loop {
        indications.signal(IndicationStyle::BlinkFast);

        let ret = central::scan(sd, &config, |params| unsafe {
            let payload = core::slice::from_raw_parts(params.data.p_data, params.data.len as usize);

            if xbox::is_xbox_controller(payload) {
                let addr = Address::new(AddressType::Public, params.peer_addr.addr);
                info!("found controller {:?}", addr);
                Some(addr)
            } else {
                None
            }
        })
        .await;

        match ret {
            Ok(addr) => return addr,
            Err(e) => {
                error!("scan error - {}", e);
                Timer::after_millis(100).await;
            }
        }
    };

    info!(
        "scanning for Xbox controllers (timeout is {}s)...",
        timeout.as_secs()
    );

    match select(do_scan(), Timer::after(timeout)).await {
        Either::First(address) => Some(address),
        Either::Second(_) => {
            warn!("scanning timed out");
            None
        }
    }
}

async fn connect(
    sd: &Softdevice,
    addr: Address,
    bonder: &'static Bonder,
    indications: &'static LedIndicationsSignal,
) -> Result<ble::Connection, BleError> {
    let whitelist = &[&addr];
    let mut config = central::ConnectConfig::default();
    config.scan_config.whitelist = Some(whitelist);

    info!("connecting to device.. {}", addr);

    indications.signal(IndicationStyle::BlinkSlow);

    let conn = central::connect_with_security(sd, &config, bonder).await?;
    match conn.encrypt() {
        Ok(_) => info!("connection encrypted!"),

        Err(EncryptError::PeerKeysNotFound) => {
            info!("no peer keys, request pairing");

            match conn.request_pairing() {
                Ok(_) => info!("pairing done"),
                Err(e) => error!("pairing not done {}", e),
            }
        }

        Err(e) => {
            error!("unable to encrypt the connection");
            return Err(BleError::Encryption(e));
        }
    };

    Ok(conn)
}

async fn run_gatt(
    conn: ble::Connection,
    indications: &'static LedIndicationsSignal,
    state: &'static SystemState,
) -> Result<(), BleError> {
    let client: XboxHidServiceClient = gatt_client::discover(&conn).await?;

    debug!("services discovered!");

    client.hid_report_cccd_write(true).await?;

    debug!("notifications enabled!");

    // XXX: would be cool to read and dynamically parse report map
    // let report_map = client.hid_report_map_read().await?;
    // info!("report map is {:x}", report_map);

    // All ready, we're connected
    indications.signal(IndicationStyle::Disabled);

    gatt_client::run(&conn, &client, |event| match event {
        XboxHidServiceClientEvent::HidReportNotification(val) => {
            let jd = xbox::JoystickData::from_packet(&val);

            if jd.buttons.contains(xbox::ButtonFlags::BUTTON_RB) {
                cortex_m::peripheral::SCB::sys_reset();
            }

            state.add_controller_sample(jd);
        }
    })
    .await;

    Ok(())
}

pub async fn central_loop(
    sd: &'static Softdevice,
    indications: &'static LedIndicationsSignal,
    state: &'static SystemState,
    bonder: &'static Bonder,
) {
    let scan_connect = async || -> Result<(), BleError> {
        if let Some(address) = scan(sd, indications).await {
            let conn = connect(sd, address, bonder, indications).await?;

            state.set_controller_connected(true);
            let _g = guard((), |_| state.set_controller_connected(false));

            match run_gatt(conn, indications, state).await {
                Err(e) => error!("run gatt exited with error - {}", e),
                _ => {}
            }
        }

        Ok(())
    };

    loop {
        if let Err(e) = scan_connect().await {
            error!("search loop error - {}", e)
        }
    }
}
