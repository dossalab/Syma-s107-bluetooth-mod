use defmt::{error, info};
use nrf_softdevice::ble::advertisement_builder::{
    Flag, LegacyAdvertisementBuilder, LegacyAdvertisementPayload, ServiceList, ServiceUuid16,
};
use nrf_softdevice::ble::peripheral::{self};
use nrf_softdevice::{
    ble::{self, gatt_server},
    Softdevice,
};

#[nrf_softdevice::gatt_service(uuid = "180f")]
pub struct BatteryService {
    #[characteristic(uuid = "2a19", read, notify)]
    battery_level: u8,
}

#[nrf_softdevice::gatt_server]
pub struct GattServer {
    bas: BatteryService,
}

pub async fn peripheral_loop(sd: &Softdevice, server: GattServer) {
    let handle_connection = async |conn: &ble::Connection| {
        gatt_server::run(conn, &server, |e| match e {
            GattServerEvent::Bas(e) => match e {
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
