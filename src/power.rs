use defmt::{error, info};
use embassy_futures::select::{select, Either};
use embassy_nrf::gpio;

use crate::{indications::LedIndicationsSignal, ChargerResources, FuelgaugeResources};

struct Charger<'a> {
    fault: gpio::Input<'a>,
    charging: gpio::Input<'a>,
}

impl<'a> Charger<'a> {
    fn new(fault: gpio::Input<'a>, charging: gpio::Input<'a>) -> Self {
        Self { fault, charging }
    }

    fn is_failure(&mut self) -> bool {
        self.fault.is_low()
    }

    fn is_charging(&mut self) -> bool {
        self.charging.is_low()
    }

    async fn poll(&mut self) {
        let fault_fut = self.fault.wait_for_any_edge();
        let status_fut = self.charging.wait_for_any_edge();

        let r = select(fault_fut, status_fut).await;
        match r {
            Either::First(_) => {
                if self.is_failure() {
                    error!("charger failure");
                }
            }
            Either::Second(_) => {
                if self.is_charging() {
                    info!("charging started")
                } else {
                    info!("charging stop")
                }
            }
        }
    }
}

#[embassy_executor::task]
pub async fn run(
    indications: &'static LedIndicationsSignal,
    fg: FuelgaugeResources,
    charger: ChargerResources,
) {
    let fault = gpio::Input::new(charger.fault, gpio::Pull::Up);
    let charging = gpio::Input::new(charger.charging, gpio::Pull::Up);

    let mut charger = Charger::new(fault, charging);
    loop {
        charger.poll().await;
    }
}
