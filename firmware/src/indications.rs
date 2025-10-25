use core::future;

use defmt::info;
use embassy_futures::select::{select, Either};
use embassy_nrf::{gpio, Peri};
use embassy_sync::{blocking_mutex::raw::CriticalSectionRawMutex, signal::Signal};
use embassy_time::{Duration, Ticker, Timer};

use crate::LedSwitchResources;

#[derive(Copy, Clone)]
pub enum IndicationStyle {
    Disabled,
    BlinkFast,
    BlinkSlow,
}

pub type LedIndicationsSignal = Signal<CriticalSectionRawMutex, IndicationStyle>;

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
pub async fn run(signal: &'static LedIndicationsSignal, mut r: LedSwitchResources) {
    info!("led indications running...");

    let mut do_indications = async |x| match x {
        IndicationStyle::Disabled => future::pending().await,
        IndicationStyle::BlinkFast => run_blinky(r.led.reborrow(), Duration::from_secs(1)).await,
        IndicationStyle::BlinkSlow => run_blinky(r.led.reborrow(), Duration::from_secs(2)).await,
    };

    let mut style = IndicationStyle::Disabled;
    loop {
        let ret = select(signal.wait(), do_indications(style)).await;
        match ret {
            Either::First(new_style) => style = new_style,
            _ => {}
        }
    }
}
