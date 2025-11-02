use defmt::{error, info, unwrap};
use embassy_futures::join::join;
use embassy_futures::select::select;
use nrf_softdevice::ble::advertisement_builder::{
    Flag, LegacyAdvertisementBuilder, LegacyAdvertisementPayload, ServiceList, ServiceUuid16,
};
use nrf_softdevice::ble::peripheral::{self};
use nrf_softdevice::ble::Connection;
use nrf_softdevice::{
    ble::{self, gatt_server},
    Softdevice,
};

use crate::bus::{BusEvent, BusMessage, BusPublisher, BusRequest, BusSubscriber, MessageBus};

#[nrf_softdevice::gatt_service(uuid = "180f")]
pub struct BatteryService {
    #[characteristic(uuid = "2a19", read, notify)]
    battery_level: u8,
}

// bas is too limited to share everything we have
#[nrf_softdevice::gatt_service(uuid = "38924a07-23d7-43fe-af5d-9c887a089cf1")]
pub struct PowerService {
    #[characteristic(uuid = "38924a07-23d7-43fe-af5d-9c887a189cf1", read, notify)]
    charger_connected: bool,

    #[characteristic(uuid = "38924a07-23d7-43fe-af5d-9c887a289cf1", read, notify)]
    battery_voltage: u16,

    #[characteristic(uuid = "38924a07-23d7-43fe-af5d-9c887a389cf1", read, notify)]
    battery_current: i16,
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

async fn handle_connection<'a>(
    server: &GattServer,
    conn: &ble::Connection,
    publisher: &'a BusPublisher<'a>,
) {
    let handle_bas = |e| match e {
        BatteryServiceEvent::BatteryLevelCccdWrite { notifications } => {
            info!("battery notifications: {}", notifications)
        }
    };

    let handle_control = |e| match e {
        ControlServiceEvent::RebootRequestWrite(_) => {
            publisher.publish_immediate(BusMessage::Request(BusRequest::Reboot))
        }
    };

    gatt_server::run(conn, server, |e| match e {
        GattServerEvent::Bas(e) => handle_bas(e),
        GattServerEvent::Control(e) => handle_control(e),
        _ => {}
    })
    .await;
}

// 2 of these can be running simultaneously - if we don't have connection, passively wait here
// and update the service with the recent data
// if we do have connection, handle notifications as well
async fn poll_messages_loop<'a>(
    mut subscriber: BusSubscriber<'a>,
    connection: Option<&Connection>,
    server: &GattServer,
) {
    loop {
        let r = match subscriber.next_message_pure().await {
            BusMessage::Event(e) => match e {
                BusEvent::Soc(soc) => {
                    connection.map(|c| server.bas.battery_level_notify(c, &soc));
                    server.bas.battery_level_set(&soc)
                }
                BusEvent::ChargerStatus(v) => {
                    connection.map(|c| server.power.charger_connected_notify(c, &v));
                    server.power.charger_connected_set(&v)
                }
                BusEvent::BatteryVoltage(v) => {
                    connection.map(|c| server.power.battery_voltage_notify(c, &v));
                    server.power.battery_voltage_set(&v)
                }
                BusEvent::BatteryCurrent(v) => {
                    connection.map(|c| server.power.battery_current_notify(c, &v));
                    server.power.battery_current_set(&v)
                }
            },

            _ => Ok(()),
        };

        if let Err(e) = r {
            error!("unable to dispatch event - {}", e)
        }
    }
}

pub async fn peripheral_loop(sd: &Softdevice, pubsub: &'static MessageBus, server: GattServer) {
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

    let publisher = unwrap!(pubsub.publisher());

    let find_connection_loop = async || loop {
        match peripheral::advertise_connectable(sd, adv, &config).await {
            Ok(conn) => {
                select(
                    handle_connection(&server, &conn, &publisher),
                    poll_messages_loop(unwrap!(pubsub.subscriber()), Some(&conn), &server),
                )
                .await;
            }
            Err(e) => error!("unable to advertise - {}", e),
        };
    };

    join(
        find_connection_loop(),
        poll_messages_loop(unwrap!(pubsub.subscriber()), None, &server),
    )
    .await;
}
