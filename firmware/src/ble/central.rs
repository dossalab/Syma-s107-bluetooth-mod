use core::cell::Cell;

use defmt::{debug, error, info, unwrap, warn};
use embassy_futures::select::{select, Either};
use embassy_sync::blocking_mutex::raw::NoopRawMutex;
use embassy_sync::signal::Signal;
use embassy_time::{Duration, Timer};
use nrf_softdevice::{
    ble::{
        self, central, gatt_client, gatt_server,
        security::{IoCapabilities, SecurityHandler},
        Address, AddressType, EncryptError, EncryptionInfo, IdentityKey, MasterId, SecurityMode,
    },
    Softdevice,
};

use crate::state::SystemState;
use crate::types::Request;
use crate::utils::run_reported;
use crate::xbox::XboxHidServiceClient;
use crate::xbox::{self, XboxHidServiceClientEvent};

use super::errors::BleError;

#[derive(Clone, Copy)]
struct Peer {
    master_id: MasterId,
    key: EncryptionInfo,
    peer_id: IdentityKey,
}

pub struct Bonder {
    peer: Cell<Option<Peer>>,
    secured: Signal<NoopRawMutex, bool>,
}

impl Default for Bonder {
    fn default() -> Self {
        Bonder {
            peer: Cell::new(None),
            secured: Signal::new(),
        }
    }
}

impl SecurityHandler for Bonder {
    fn io_capabilities(&self) -> IoCapabilities {
        IoCapabilities::None
    }

    fn can_bond(&self, _conn: &ble::Connection) -> bool {
        true
    }

    fn on_bonded(
        &self,
        _conn: &ble::Connection,
        master_id: MasterId,
        key: EncryptionInfo,
        peer_id: IdentityKey,
    ) {
        info!("storing keys");
        self.peer.set(Some(Peer {
            master_id,
            key,
            peer_id,
        }));
    }

    fn on_security_update(&self, _conn: &ble::Connection, security_mode: SecurityMode) {
        match security_mode {
            SecurityMode::NoAccess | SecurityMode::Open => self.secured.signal(false),
            _ => self.secured.signal(true),
        }
    }

    fn save_sys_attrs(&self, _conn: &ble::Connection) {}

    fn get_key(&self, _conn: &ble::Connection, master_id: MasterId) -> Option<EncryptionInfo> {
        self.peer
            .get()
            .and_then(|peer| (master_id == peer.master_id).then_some(peer.key))
    }

    fn get_peripheral_key(&self, conn: &ble::Connection) -> Option<(MasterId, EncryptionInfo)> {
        self.peer.get().and_then(|peer| {
            peer.peer_id
                .is_match(conn.peer_address())
                .then_some((peer.master_id, peer.key))
        })
    }
}

// Scan for Xbox controllers
async fn scan(sd: &Softdevice) -> Option<Address> {
    let config = central::ScanConfig {
        interval: 3200, // *0.625 us
        window: 160,    // *0.625us
        ..central::ScanConfig::default()
    };

    let timeout = Duration::from_secs(60);

    let do_scan = async || loop {
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
) -> Result<ble::Connection, BleError> {
    bonder.secured.reset();

    let whitelist = &[&addr];
    let mut config = central::ConnectConfig::default();
    config.scan_config.whitelist = Some(whitelist);

    info!("connecting to device.. {}", addr);

    let conn = central::connect_with_security(sd, &config, bonder).await?;

    let secured = match conn.encrypt() {
        Ok(()) => {
            if bonder.secured.wait().await {
                true
            } else {
                warn!("encryption with stored keys failed, requesting pairing");
                if let Err(e) = conn.request_pairing() {
                    error!("failed to initiate pairing: {}", e);
                    return Err(BleError::SecurityFailed);
                }
                bonder.secured.wait().await
            }
        }
        Err(EncryptError::PeerKeysNotFound) => {
            info!("no peer keys, requesting pairing");
            if let Err(e) = conn.request_pairing() {
                error!("failed to initiate pairing: {}", e);
                return Err(BleError::SecurityFailed);
            }
            bonder.secured.wait().await
        }
        Err(e) => {
            error!("unable to initiate encryption: {}", e);
            return Err(BleError::Encryption(e));
        }
    };

    if !secured {
        error!("failed to secure connection");
        return Err(BleError::SecurityFailed);
    }

    info!("connection secured!");

    if let Err(e) = gatt_server::set_sys_attrs(&conn, None) {
        error!("set_sys_attrs failed - {}", e);
        return Err(BleError::SecurityFailed);
    }

    Ok(conn)
}

async fn run_gatt(conn: ble::Connection, state: &'static SystemState) -> Result<(), BleError> {
    let controller_sample_sender = state.controller_sample.sender();
    let client: XboxHidServiceClient = gatt_client::discover(&conn).await?;

    debug!("services discovered!");

    let report = client.hid_report_map_read().await?;
    client.hid_report_cccd_write(true).await?;
    info!("report map: {}", report);

    debug!("notifications enabled!");

    // XXX: would be cool to read and dynamically parse report map
    // let report_map = client.hid_report_map_read().await?;
    // info!("report map is {:x}", report_map);

    // All setup done - mark as connected now
    run_reported(state.controller_connected.sender(), async {
        gatt_client::run(&conn, &client, |event| match event {
            XboxHidServiceClientEvent::HidReportNotification(val) => {
                let jd = xbox::decode_hid_report(&val);
                controller_sample_sender.send(jd);
            }
        })
        .await;
    })
    .await;

    Ok(())
}

pub async fn central_loop(
    sd: &'static Softdevice,
    state: &'static SystemState,
    bonder: &'static Bonder,
) {
    let mut requests_receiver = unwrap!(state.requests.receiver());

    loop {
        // Wait for a StartScan request before doing anything
        loop {
            if matches!(requests_receiver.changed().await, Request::StartScan) {
                break;
            }
        }

        info!("StartScan request received, scanning...");

        let result: Result<(), BleError> = async {
            let address = run_reported(state.scan_state.sender(), scan(sd)).await;

            if let Some(address) = address {
                let conn = connect(sd, address, bonder).await?;

                if let Err(e) = run_gatt(conn, state).await {
                    error!("run gatt exited with error - {}", e);
                }
            }

            Ok(())
        }
        .await;

        if let Err(e) = result {
            error!("scan/connect error - {}", e);
        }
    }
}
