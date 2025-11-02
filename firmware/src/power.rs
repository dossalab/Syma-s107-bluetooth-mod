use crate::{
    bus::{BusEvent, BusMessage, BusPublisher},
    PowerResources, SharedI2cBus,
};
use bq27xxx::{
    chips::bq27427::{CurrentThresholds, RaTable, StateClass},
    defs::StatusFlags,
    Bq27xx, ChemId,
};
use defmt::{error, info, warn};
use embassy_embedded_hal::shared_bus::asynch::i2c::I2cDevice;
use embassy_futures::select::{select, Either};
use embassy_nrf::gpio;
use embassy_time::Timer;
use embedded_hal_async::i2c;

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

    async fn poll(&mut self, publisher: &'a BusPublisher<'a>) {
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
                let is_charging = self.is_charging();
                publisher
                    .publish_immediate(BusMessage::Event(BusEvent::ChargerStatus(is_charging)));

                if is_charging {
                    info!("charging started")
                } else {
                    info!("charging stop")
                }
            }
        }
    }
}

struct Fuelgauge<'a, E, I: i2c::I2c<Error = E>> {
    int: gpio::Input<'a>,
    gauge: Bq27xx<I, embassy_time::Delay>,
}

impl<'a, E, I> Fuelgauge<'a, E, I>
where
    I: i2c::I2c<Error = E>,
{
    async fn update_power_stats(
        &mut self,
        publisher: &BusPublisher<'a>,
    ) -> Result<(), bq27xxx::ChipError<E>> {
        let soc = self.gauge.state_of_charge().await?;
        let voltage = self.gauge.voltage().await?;
        let current = self.gauge.average_current().await?;

        info!(
            "SoC: {} %, V: {} mV, I: {} mA [{}]",
            soc,
            voltage,
            current,
            self.gauge.get_flags().await?
        );

        publisher.publish_immediate(BusMessage::Event(BusEvent::Soc(soc as u8)));
        publisher.publish_immediate(BusMessage::Event(BusEvent::BatteryVoltage(voltage)));
        publisher.publish_immediate(BusMessage::Event(BusEvent::BatteryCurrent(current)));

        Ok(())
    }

    async fn wait_int(
        &mut self,
        publisher: &BusPublisher<'a>,
    ) -> Result<(), bq27xxx::ChipError<E>> {
        let r = select(self.int.wait_for_falling_edge(), Timer::after_secs(1)).await;

        match r {
            Either::First(_) => {
                info!("fuelgauge interrupt");

                let soc = self.gauge.state_of_charge().await?;
                publisher.publish_immediate(BusMessage::Event(BusEvent::Soc(soc as u8)));
            }

            // Timer interrupt
            Either::Second(_) => self.update_power_stats(publisher).await?,
        };

        Ok(())
    }

    async fn write_memory_params(&mut self) -> Result<(), bq27xxx::ChipError<E>> {
        let start_learning = false;

        info!("updating fuelgauge memory...");

        self.gauge
            .memory_modify(|b: &mut StateClass| {
                b.set_capacity(200);
                b.set_energy(740); // capacity * 3.7
                b.set_terminate_voltage(3200); // mV

                // Taper Rate = Design Capacity / (0.1 × taper current)
                // XXX: This assumes charge current is 100 mA, taper current is 25 ma
                // npm1100 seems to come closer to 20 ma, then switches to 10 ma for 300ms, then drops to 0
                b.set_taper_rate(75);

                if start_learning {
                    b.set_update_status(0x03);
                }

                // Learned value
                b.set_qmax(17449);
            })
            .await?;

        self.gauge
            .memory_modify(|b: &mut CurrentThresholds| {
                b.set_discharge_current_threshold(400);
                b.set_quit_current_threshold(200);
            })
            .await?;

        self.gauge
            .memory_modify(|b: &mut RaTable| {
                // This is obtained from learning cycle :)
                b.set_points([50, 30, 34, 46, 38, 32, 37, 31, 32, 35, 39, 39, 61, 115, 200]);
            })
            .await
    }

    async fn probe(&mut self) -> Result<(), bq27xxx::ChipError<E>> {
        let force_memory_update = false;

        self.gauge.probe().await?;

        match self.gauge.read_chem_id().await? {
            ChemId::B4200 => info!("fuelgauge chem id correct"),
            _ => {
                warn!("chem id is not set, applying");
                self.gauge.write_chem_id(ChemId::B4200).await?;
            }
        }

        let flags = self.gauge.get_flags().await?;

        if flags.contains(StatusFlags::ITPOR) || force_memory_update {
            info!("fuelgauge ITPOR condition");
            self.write_memory_params().await?;
        }

        info!("state: {}", self.gauge.memblock_read::<StateClass>().await?);

        info!("ratable: {}", self.gauge.memblock_read::<RaTable>().await?);

        Ok(())
    }

    fn new(int: gpio::Input<'a>, i2c: I) -> Self {
        const GAUGE_I2C_ADDR: u8 = 0x55;
        Self {
            int,
            gauge: Bq27xx::new(i2c, embassy_time::Delay, GAUGE_I2C_ADDR),
        }
    }
}

#[embassy_executor::task]
pub async fn run(publisher: BusPublisher<'static>, r: PowerResources, i2c: &'static SharedI2cBus) {
    let charger_fault = gpio::Input::new(r.fault_int, gpio::Pull::Up);
    let charger_charging = gpio::Input::new(r.charging_int, gpio::Pull::Up);
    let fuelgauge_int = gpio::Input::new(r.fuelgauge_int, gpio::Pull::Up);

    let dev = I2cDevice::new(i2c);

    info!("running power task");

    let mut charger = Charger::new(charger_fault, charger_charging);
    let mut fuelgauge = Fuelgauge::new(fuelgauge_int, dev);

    if let Err(e) = fuelgauge.probe().await {
        error!("fuelgauge initialization failure - {}", e);
    }

    loop {
        select(charger.poll(&publisher), fuelgauge.wait_int(&publisher)).await;
    }
}
