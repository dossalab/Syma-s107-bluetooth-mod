use core::future;

use defmt::{info, unwrap};
use embassy_futures::select::{select4, Either4};
use embassy_nrf::{gpio, Peri};
use embassy_time::{Duration, Ticker, Timer};

use crate::{state::SystemState, LedSwitchResources};

#[derive(Copy, Clone)]
pub enum IndicationStyle {
    Disabled,
    BlinkInterval(Duration),
    BlinkOnce,
}

async fn run_single_blink<'a, P>(pin: Peri<'a, P>)
where
    P: gpio::Pin,
{
    let mut output = gpio::Output::new(pin, gpio::Level::Low, gpio::OutputDrive::Standard);

    output.set_high();
    Timer::after_millis(50).await;
    output.set_low();
}

async fn run_blinky<'a, P>(pin: Peri<'a, P>, period: Duration)
where
    P: gpio::Pin,
{
    let mut output = gpio::Output::new(pin, gpio::Level::Low, gpio::OutputDrive::Standard);
    let mut ticker = Ticker::every(period);

    loop {
        output.set_high();
        Timer::after_millis(50).await;
        output.set_low();

        ticker.next().await;
    }
}

#[embassy_executor::task]
pub async fn run(state: &'static SystemState, mut r: LedSwitchResources) {
    info!("led indications running...");

    let mut soc_receiver = unwrap!(state.soc.receiver());
    let mut charger_state_receiver = unwrap!(state.charger_state.receiver());
    let mut controller_connection_receiver = unwrap!(state.charger_state.receiver());

    let mut do_indications = async |x| match x {
        IndicationStyle::Disabled => future::pending().await,
        IndicationStyle::BlinkInterval(interval) => run_blinky(r.led.reborrow(), interval).await,
        IndicationStyle::BlinkOnce => run_single_blink(r.led.reborrow()).await,
    };

    let mut style = IndicationStyle::Disabled;

    loop {
        let s = select4(
            soc_receiver.changed(),
            charger_state_receiver.changed(),
            controller_connection_receiver.changed(),
            do_indications(style),
        )
        .await;

        style = match s {
            Either4::First(_) => IndicationStyle::BlinkOnce,
            Either4::Second(_) => IndicationStyle::BlinkOnce,
            Either4::Third(_) => IndicationStyle::BlinkInterval(Duration::from_secs(1)),
            Either4::Fourth(_) => IndicationStyle::Disabled,
        };
    }
}
