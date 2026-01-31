use defmt::{info, unwrap};
use embassy_futures::select::select3;
use embassy_sync::{
    blocking_mutex::raw::NoopRawMutex,
    watch::{Receiver, Watch},
};

use crate::types::{ChargerState, JoystickData, PeriodicUpdate, PidUpdate};

pub type StateWatch<T> = Watch<NoopRawMutex, T, 8>;
pub type StateReceiver<'a, T> = Receiver<'a, NoopRawMutex, T, 8>;

pub struct SystemState {
    pub charger_state: StateWatch<ChargerState>,
    pub soc: StateWatch<u8>,
    pub controller_connected: StateWatch<bool>,
    pub periodic_update: StateWatch<PeriodicUpdate>,
    pub controller_sample: StateWatch<JoystickData>,
    pub pid_update: StateWatch<PidUpdate>,
    pub controller_run_allowed: StateWatch<bool>,
}

impl<'a> SystemState {
    pub fn new() -> Self {
        Self {
            charger_state: Watch::new(),
            soc: Watch::new(),
            controller_connected: Watch::new_with(false),
            periodic_update: Watch::new(),
            controller_sample: Watch::new(),
            pid_update: Watch::new(),
            controller_run_allowed: Watch::new_with(false),
        }
    }
}

#[embassy_executor::task]
pub async fn run(state: &'static SystemState) {
    // Monitor the overall state of the system and provide a couple of
    // our own statuses as well
    info!("system state monitor running");

    let mut soc_receiver = unwrap!(state.soc.receiver());
    let mut controller_connected_receiver = unwrap!(state.controller_connected.receiver());
    let mut charger_state_receiver = unwrap!(state.charger_state.receiver());
    let controller_run_allowed_sender = state.controller_run_allowed.sender();

    loop {
        controller_run_allowed_sender.send(matches!(
            (soc_receiver.try_get(), controller_connected_receiver.try_get(), charger_state_receiver.try_get()),
            (Some(soc), Some(true), Some(charger_state)) if soc > 5 && !charger_state.charging
        ));

        select3(
            soc_receiver.changed(),
            controller_connected_receiver.changed(),
            charger_state_receiver.changed(),
        )
        .await;
    }
}
