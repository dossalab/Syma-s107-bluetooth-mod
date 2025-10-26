use embassy_sync::{
    blocking_mutex::raw::CriticalSectionRawMutex,
    pubsub::{PubSubChannel, Publisher, Subscriber},
};

#[derive(Clone)]
pub enum BusRequest {
    Reboot,
}

#[derive(Clone)]
pub enum BusEvent {
    Soc(u8),
    BatteryVoltage(u32),
    ChargerStatus(bool),
}

#[derive(Clone)]
pub enum BusMessage {
    Request(BusRequest),
    Event(BusEvent),
}

pub type MessageBus = PubSubChannel<CriticalSectionRawMutex, BusMessage, 4, 4, 4>;
pub type BusPublisher<'a> = Publisher<'a, CriticalSectionRawMutex, BusMessage, 4, 4, 4>;
pub type BusSubscriber<'a> = Subscriber<'a, CriticalSectionRawMutex, BusMessage, 4, 4, 4>;
