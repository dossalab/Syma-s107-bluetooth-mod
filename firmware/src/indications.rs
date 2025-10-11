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

    let sine_sequence = [
        500, 598, 691, 778, 854, 916, 962, 990, 1000, 990, 962, 916, 854, 778, 691, 598, 500, 402,
        309, 222, 146, 84, 38, 10, 0, 10, 38, 84, 146, 222, 309, 402,
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
