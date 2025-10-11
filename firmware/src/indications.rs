use core::future;

use defmt::{error, info, unwrap};
use embassy_futures::select::{select, Either};
use embassy_nrf::pwm::{self, SequenceConfig, SequencePwm, SingleSequenceMode, SingleSequencer};
use embassy_sync::{blocking_mutex::raw::CriticalSectionRawMutex, signal::Signal};

use crate::LedSwitchResources;

#[derive(Copy, Clone)]
pub enum IndicationStyle {
    Disabled,
    BlinkFast,
    BlinkSlow,
}

pub type LedIndicationsSignal = Signal<CriticalSectionRawMutex, IndicationStyle>;

async fn run_sequencer<'a>(
    pwm: &mut SequencePwm<'a>,
    sequence: &[u16],
    refresh: u32,
) -> Result<(), pwm::Error> {
    let mut sequence_config = SequenceConfig::default();
    sequence_config.refresh = refresh;

    let sequence = SingleSequencer::new(pwm, sequence, sequence_config);
    sequence.start(SingleSequenceMode::Infinite)?;

    Ok(future::pending().await)
}

#[embassy_executor::task]
pub async fn run(signal: &'static LedIndicationsSignal, r: LedSwitchResources) {
    info!("led indications running...");

    let pwm_config = pwm::Config::default();
    let mut pwm = unwrap!(SequencePwm::new_1ch(r.pwm, r.led, pwm_config));

    // Bit 15 here is used to set PWM output polarity
    let sine_sequence = [
        0x8000, 0x8062, 0x80c3, 0x8122, 0x817f, 0x81d7, 0x822c, 0x827a, 0x82c3, 0x8305, 0x833f,
        0x8372, 0x839c, 0x83bd, 0x83d5, 0x83e3, 0x83e8, 0x83e3, 0x83d5, 0x83bd, 0x839c, 0x8372,
        0x833f, 0x8305, 0x82c3, 0x827a, 0x822c, 0x81d7, 0x817f, 0x8122, 0x80c3, 0x8062, 0x8000,
    ];

    let mut do_indications = async |x| match x {
        IndicationStyle::Disabled => future::pending().await,
        IndicationStyle::BlinkFast => run_sequencer(&mut pwm, &sine_sequence, 20).await,
        IndicationStyle::BlinkSlow => run_sequencer(&mut pwm, &sine_sequence, 40).await,
    };

    let mut style = IndicationStyle::Disabled;

    loop {
        let ret = select(signal.wait(), do_indications(style)).await;
        match ret {
            Either::First(new_style) => style = new_style,
            Either::Second(r) => {
                if r.is_err() {
                    error!("unable to start new sequence");
                }
            }
        }
    }
}
