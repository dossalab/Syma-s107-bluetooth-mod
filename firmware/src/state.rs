use core::sync::atomic::{AtomicBool, AtomicU8, Ordering};

use defmt::unwrap;
use embassy_futures::select::select;
use embassy_sync::{
    blocking_mutex::raw::NoopRawMutex,
    watch::{Receiver, Watch},
};

use crate::xbox::JoystickData;
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
    GyroSample(i16),
    PidUpdate(f32, f32, f32),
    ControllerConnection(bool),
    ControllerData(JoystickData),
}

type StatsWatch = Watch<NoopRawMutex, UpdateType, 8>;
type StatsReceiver<'a> = Receiver<'a, NoopRawMutex, UpdateType, 8>;

pub struct SystemState {
    charging: AtomicBool,
    charger_failure: AtomicBool,
    soc: AtomicU8,
    controller_connected: AtomicBool,

    watch: StatsWatch,
}

impl<'a> SystemState {
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

    pub fn add_gyro_sample(&self, s: i16) {
        self.notify(UpdateType::GyroSample(s));
    }

    pub fn set_charging(&self, charging: bool) {
        self.charging.store(charging, Ordering::Relaxed);
        self.notify(UpdateType::ChargingStatus(charging));
    }

    pub fn set_charger_failure(&self, failure: bool) {
        self.charger_failure.store(failure, Ordering::Relaxed);
        self.notify(UpdateType::ChargingFailure(failure));
    }

    pub fn update_controller_pid(&self, p: f32, i: f32, d: f32) {
        self.notify(UpdateType::PidUpdate(p, i, d));
    }

    pub fn set_controller_connected(&self, connected: bool) {
        self.controller_connected
            .store(connected, Ordering::Relaxed);
        self.notify(UpdateType::ControllerConnection(connected));
    }

    pub fn is_controller_connected(&self) -> bool {
        self.controller_connected.load(Ordering::Relaxed)
    }

    pub fn add_controller_sample(&self, j: JoystickData) {
        self.notify(UpdateType::ControllerData(j));
    }

    pub async fn run_while<C, F>(&self, cond: C, mut fun: F)
    where
        C: Fn() -> bool,
        F: AsyncFnMut(),
    {
        let mut receiver = unwrap!(self.event_receiver());

        loop {
            let mut wait_cancellation = async || loop {
                _ = receiver.changed().await;
                if !cond() {
                    break;
                }
            };

            if cond() {
                select(fun(), wait_cancellation()).await;
                continue;
            }

            _ = receiver.changed().await;
        }
    }

    pub fn new() -> Self {
        Self {
            charger_failure: Default::default(),
            charging: Default::default(),
            soc: Default::default(),
            controller_connected: Default::default(),

            watch: Watch::new(),
        }
    }
}
