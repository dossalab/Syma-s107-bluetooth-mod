use defmt::{debug, error, info, unwrap, warn};
use embassy_futures::join::join3;
use embassy_futures::select::{select, Either};
use embassy_time::{Duration, Timer};
use nrf_softdevice::ble::advertisement_builder::{
    Flag, LegacyAdvertisementBuilder, LegacyAdvertisementPayload, ServiceList, ServiceUuid16,
};
use nrf_softdevice::ble::peripheral::{self, AdvertiseError};
use nrf_softdevice::{
    ble::{
        self,
        central::{self, ConnectError},
        gatt_client::{self, DiscoverError},
        gatt_server,
        security::SecurityHandler,
        Address, AddressType, EncryptError, EncryptionInfo,
    },
    Softdevice,
};
use static_cell::StaticCell;

use crate::{ble_events::BluetoothEventsProxy, xbox::XboxHidServiceClient};
use crate::{
    indications::{IndicationStyle, LedIndicationsSignal},
    xbox::{self, XboxHidServiceClientEvent},
};

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

#[cfg_attr(feature = "defmt", derive(defmt::Format))]
enum BleError {
    Encryption(ble::EncryptError),
    ConnectError(ConnectError),
    DiscoveryError,
    WriteError(gatt_client::WriteError),
    ReadError(gatt_client::ReadError),
    AdvertiseError(AdvertiseError),
}

impl From<ConnectError> for BleError {
    fn from(e: ConnectError) -> Self {
        return Self::ConnectError(e);
    }
}

impl From<DiscoverError> for BleError {
    fn from(_: DiscoverError) -> Self {
        return Self::DiscoveryError;
    }
}

impl From<gatt_client::WriteError> for BleError {
    fn from(e: gatt_client::WriteError) -> Self {
        return Self::WriteError(e);
    }
}

impl From<gatt_client::ReadError> for BleError {
    fn from(e: gatt_client::ReadError) -> Self {
        return Self::ReadError(e);
    }
}

impl From<AdvertiseError> for BleError {
    fn from(e: AdvertiseError) -> Self {
        return Self::AdvertiseError(e);
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

async fn run_services(
    conn: ble::Connection,
    proxy: &'static BluetoothEventsProxy,
    indications: &'static LedIndicationsSignal,
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

            proxy.notify_data(jd);
        }
    })
    .await;

    Ok(())
}

#[nrf_softdevice::gatt_service(uuid = "180f")]
struct BatteryService {
    #[characteristic(uuid = "2a19", read, notify)]
    battery_level: u8,
}

#[nrf_softdevice::gatt_server]
struct TelemetryServer {
    bas: BatteryService,
}

async fn peripheral_loop(sd: &Softdevice, server: TelemetryServer) {
    let handle_connection = async |conn: &ble::Connection| {
        gatt_server::run(conn, &server, |e| match e {
            TelemetryServerEvent::Bas(e) => match e {
                BatteryServiceEvent::BatteryLevelCccdWrite { notifications } => {
                    info!("battery notifications: {}", notifications)
                }
            },
        })
        .await;
    };

    static ADV_DATA: LegacyAdvertisementPayload = LegacyAdvertisementBuilder::new()
        .flags(&[Flag::GeneralDiscovery, Flag::LE_Only])
        .services_16(ServiceList::Complete, &[ServiceUuid16::HEALTH_THERMOMETER])
        .short_name("Hello")
        .build();

    let config = peripheral::Config {
        interval: 1600, // * 0.625us
        ..peripheral::Config::default()
    };

    let adv = peripheral::ConnectableAdvertisement::ScannableUndirected {
        adv_data: &ADV_DATA,
        scan_data: &SCAN_DATA,
    };

    static SCAN_DATA: LegacyAdvertisementPayload = LegacyAdvertisementBuilder::new()
        .full_name("Hello, Rust!")
        .build();

    loop {
        match peripheral::advertise_connectable(sd, adv, &config).await {
            Ok(conn) => handle_connection(&conn).await,
            Err(e) => error!("unable to advertise - {}", e),
        };
    }
}

async fn central_loop(
    sd: &'static Softdevice,
    indications: &'static LedIndicationsSignal,
    events: &'static BluetoothEventsProxy,
    bonder: &'static Bonder,
) {
    let scan_connect = async || -> Result<(), BleError> {
        if let Some(address) = scan(sd, indications).await {
            let connection = connect(sd, address, bonder, indications).await?;
            events
                .notify_connection(async || run_services(connection, events, indications).await)
                .await?;
        }

        Ok(())
    };

    loop {
        if let Err(e) = scan_connect().await {
            error!("search loop error - {}", e)
        }
    }
}

#[embassy_executor::task]
pub async fn run(
    sd: &'static mut Softdevice,
    indications: &'static LedIndicationsSignal,
    events: &'static BluetoothEventsProxy,
) {
    static BONDER: StaticCell<Bonder> = StaticCell::new();
    let bonder = BONDER.init(Bonder::default());
    let server = unwrap!(TelemetryServer::new(sd));

    join3(
        central_loop(sd, indications, events, bonder),
        peripheral_loop(sd, server),
        sd.run(),
    )
    .await;
}
