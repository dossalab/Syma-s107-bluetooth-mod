use defmt::{info, unwrap};
use embassy_futures::select::select3;
use embassy_nrf::gpio;
use embassy_time::Timer;

use crate::{state::SystemState, LedSwitchResources};

#[embassy_executor::task]
pub async fn run(state: &'static SystemState, r: LedSwitchResources) {
    info!("led indications running...");

    let mut soc_receiver = unwrap!(state.soc.receiver());
    let mut charger_state_receiver = unwrap!(state.charger_state.receiver());
    let mut controller_connection_receiver = unwrap!(state.controller_connected.receiver());

    let mut output = gpio::Output::new(r.led, gpio::Level::Low, gpio::OutputDrive::Standard);

    loop {
        // Just blink once per each monitored event for now
        output.set_high();
        Timer::after_millis(50).await;
        output.set_low();

        select3(
            soc_receiver.changed(),
            charger_state_receiver.changed(),
            controller_connection_receiver.changed(),
        )
        .await;
    }
}
