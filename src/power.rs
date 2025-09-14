use crate::{indications::LedIndicationsSignal, ChargerResources, FuelgaugeResources, Irqs};
use bq27xxx::{Bq27xx, ChemId};
use defmt::{error, info, warn};
use embassy_futures::select::{select, Either};
use embassy_nrf::{
    gpio,
    twim::{self, Twim},
};

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

struct Fuelgauge<'a, P: twim::Instance> {
    int: gpio::Input<'a>,
    gauge: Bq27xx<Twim<'a, P>, embassy_time::Delay>,
}

impl<'a, P: twim::Instance> Fuelgauge<'a, P> {
    async fn poll(&mut self) -> Result<(), bq27xxx::ChipError<twim::Error>> {
        self.int.wait_for_any_edge().await;

        info!("fuelgauge interrupt");

        let soc = self.gauge.state_of_charge().await?;
        info!("state of charge: {} %", soc);

        Ok(())
    }

    async fn probe(&mut self) -> Result<(), bq27xxx::ChipError<twim::Error>> {
        self.gauge.probe().await?;

        match self.gauge.read_chem_id().await? {
            ChemId::B4200 => info!("fuelgauge chem id correct"),
            _ => {
                warn!("chem id is not set, applying");
                self.gauge.write_chem_id(ChemId::B4200).await?;
            }
        }

        info!("battery voltage: {} mV", self.gauge.voltage().await?);
        info!("state of charge: {} %", self.gauge.state_of_charge().await?);
        info!("flags are [{}]", self.gauge.get_flags().await?);

        Ok(())
    }

    fn new(int: gpio::Input<'a>, i2c: twim::Twim<'a, P>) -> Self {
        const GAUGE_I2C_ADDR: u8 = 0x55;
        Self {
            int,
            gauge: Bq27xx::new(i2c, embassy_time::Delay, GAUGE_I2C_ADDR),
        }
    }
}

#[embassy_executor::task]
pub async fn run(
    indications: &'static LedIndicationsSignal,
    fg: FuelgaugeResources,
    charger: ChargerResources,
) {
    let charger_fault = gpio::Input::new(charger.fault, gpio::Pull::Up);
    let charger_charging = gpio::Input::new(charger.charging, gpio::Pull::Up);
    let fuelgauge_int = gpio::Input::new(fg.int, gpio::Pull::Up);

    let mut i2c_buffer = [0; 64];
    let i2c = Twim::new(
        fg.i2c,
        Irqs,
        fg.sda,
        fg.scl,
        twim::Config::default(),
        &mut i2c_buffer,
    );

    info!("running power task");

    let mut charger = Charger::new(charger_fault, charger_charging);
    let mut fuelgauge = Fuelgauge::new(fuelgauge_int, i2c);

    if fuelgauge.probe().await.is_err() {
        error!("fuelgauge initialization failure");
    }

    loop {
        select(charger.poll(), fuelgauge.poll()).await;
    }
}
