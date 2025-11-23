use core::sync::atomic::{AtomicBool, AtomicU8, Ordering};

use embassy_sync::{
    blocking_mutex::raw::CriticalSectionRawMutex,
    watch::{Receiver, Watch},
};

// Use simple packing to help with BLE serialization later
#[repr(C, packed)]
#[derive(Default, Copy, Clone)]
pub struct PeriodicUpdate {
    pub voltage: u16,
    pub current: i16,
    pub temperature: u16,
}

#[derive(Clone)]
pub enum UpdateType {
    Soc(u8),
    ChargingStatus(bool),
    ChargingFailure(bool),
    PeriodicUpdate(PeriodicUpdate),
}

type StatsWatch = Watch<CriticalSectionRawMutex, UpdateType, 2>;
type StatsReceiver<'a> = Receiver<'a, CriticalSectionRawMutex, UpdateType, 2>;

pub struct PowerStats {
    charging: AtomicBool,
    charger_failure: AtomicBool,
    soc: AtomicU8,

    watch: StatsWatch,
}

impl<'a> PowerStats {
    pub fn event_receiver(&'a self) -> Option<StatsReceiver<'a>> {
        self.watch.receiver()
    }

    pub fn is_charging(&self) -> bool {
        self.charging.load(Ordering::Relaxed)
    }

    pub fn is_charging_failure(&self) -> bool {
        self.charger_failure.load(Ordering::Relaxed)
    }

    pub fn soc(&self) -> u8 {
        self.soc.load(Ordering::Relaxed)
    }

    pub fn is_soc_fatal(&self) -> bool {
        self.soc() <= 5
    }

    pub fn is_soc_low(&self) -> bool {
        self.soc() <= 15
    }

    fn notify(&self, t: UpdateType) {
        self.watch.sender().send(t);
    }

    pub fn add_soc(&self, soc: u8) {
        self.soc.store(soc, Ordering::Relaxed);
        self.notify(UpdateType::Soc(soc));
    }

    pub fn add_periodic_update(&self, u: PeriodicUpdate) {
        self.notify(UpdateType::PeriodicUpdate(u));
    }

    pub fn set_charging(&self, charging: bool) {
        self.charging.store(charging, Ordering::Relaxed);
        self.notify(UpdateType::ChargingStatus(charging));
    }

    pub fn set_charger_failure(&self, failure: bool) {
        self.charger_failure.store(failure, Ordering::Relaxed);
        self.notify(UpdateType::ChargingFailure(failure));
    }

    pub fn new() -> Self {
        Self {
            charger_failure: Default::default(),
            charging: Default::default(),
            soc: Default::default(),

            watch: Watch::new(),
        }
    }
}
