use crate::{indications::LedIndicationsSignal, PowerResources, SharedI2cBus};
use bq27xxx::{memory::MemoryBlock, Bq27xx, ChemId};
use defmt::{debug, error, info, warn};
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

mod blockdefs {
    pub const CURRENT_THRESHOLDS: u8 = 81;
    pub const STATE_CLASS: u8 = 82;

    pub const QMAX: usize = 0;
    pub const UPDATE_STATUS: usize = 2;
    pub const CAPACITY: usize = 6;
    pub const ENERGY: usize = 8;
    pub const TERMINATE_VOLTAGE: usize = 10;
    pub const TAPER_RATE: usize = 21;
}

struct Fuelgauge<'a, E, I: i2c::I2c<Error = E>> {
    int: gpio::Input<'a>,
    gauge: Bq27xx<I, embassy_time::Delay>,
}

impl<'a, E, I> Fuelgauge<'a, E, I>
where
    I: i2c::I2c<Error = E>,
{
    async fn print_stats(&mut self) -> Result<(), bq27xxx::ChipError<E>> {
        info!("battery voltage: {} mV", self.gauge.voltage().await?);
        info!(
            "battery current: {} mA",
            self.gauge.average_current().await?
        );
        info!("state of charge: {} %", self.gauge.state_of_charge().await?);
        info!("flags are [{}]", self.gauge.get_flags().await?);

        Ok(())
    }

    async fn wait_int(&mut self) -> Result<(), bq27xxx::ChipError<E>> {
        let r = select(self.int.wait_for_falling_edge(), Timer::after_secs(1)).await;

        match r {
            Either::First(_) => {
                info!("fuelgauge interrupt");
            }
            Either::Second(_) => {}
        }

        self.print_stats().await
    }

    fn write_u16(block: &mut MemoryBlock, at: usize, value: u16) {
        block.raw[at] = (value >> 8) as u8;
        block.raw[at + 1] = value as u8;
    }

    fn read_u16(block: &MemoryBlock, at: usize) -> u16 {
        ((block.raw[at] as u16) << 8) | block.raw[at + 1] as u16
    }

    fn set_capacity(block: &mut MemoryBlock, capacity: u16) {
        Self::write_u16(block, blockdefs::CAPACITY, capacity)
    }

    fn get_capacity(block: &MemoryBlock) -> u16 {
        Self::read_u16(&block, blockdefs::CAPACITY)
    }

    fn set_energy(block: &mut MemoryBlock, energy: u16) {
        Self::write_u16(block, blockdefs::ENERGY, energy)
    }

    fn get_energy(block: &MemoryBlock) -> u16 {
        Self::read_u16(block, blockdefs::ENERGY)
    }

    fn set_taper_rate(block: &mut MemoryBlock, rate: u16) {
        Self::write_u16(block, blockdefs::TAPER_RATE, rate)
    }

    fn get_taper_rate(block: &MemoryBlock) -> u16 {
        Self::read_u16(block, blockdefs::TAPER_RATE)
    }

    fn set_terminate_voltage(block: &mut MemoryBlock, v: u16) {
        Self::write_u16(block, blockdefs::TERMINATE_VOLTAGE, v)
    }

    fn get_terminate_voltage(block: &MemoryBlock) -> u16 {
        Self::read_u16(block, blockdefs::TERMINATE_VOLTAGE)
    }

    fn get_qmax(block: &MemoryBlock) -> u16 {
        Self::read_u16(block, blockdefs::QMAX)
    }

    fn get_update_status(block: &MemoryBlock) -> u8 {
        block.raw[blockdefs::UPDATE_STATUS]
    }

    fn set_update_status(block: &mut MemoryBlock, status: u8) {
        block.raw[blockdefs::UPDATE_STATUS] = status;
    }

    async fn set_battery_state(&mut self) -> Result<(), bq27xxx::ChipError<E>> {
        const BATTERY_CAPACITY: u16 = 500;
        const BATTERY_ENERGY: u16 = 1850; // capacity * 3.7
        const BATTERY_TERM_V: u16 = 3600; // could be lower, but I doubt we'll be able to fly at such low voltages
        const LEARNING_UPDATE_STATUS: u8 = 0x03;

        let start_learning = false;

        // Taper Rate = Design Capacity / (0.1 × taper current)
        // XXX: This assumes charge current is 100 mA, taper current is 20 ma (+- 10%)
        // npm1100 seems to come closer to 20 ma, then switches to 10 ma for 300ms, then drops to 0
        const BATTERY_TAPER_RATE: u16 = 227;

        let mut block = self.gauge.memblock_read(blockdefs::STATE_CLASS, 0).await?;

        debug!("block: {}", block.raw);

        let capacity = Self::get_capacity(&block);
        let energy = Self::get_energy(&block);
        let taper_rate = Self::get_taper_rate(&block);
        let terminate_voltage = Self::get_terminate_voltage(&block);
        let update_status = Self::get_update_status(&block);

        info!("block capacity: {} mAh", capacity);
        info!("block energy: {} mWh", energy);
        info!("taper rate: {} .1 Hr", taper_rate);
        info!("terminate voltage : {} mV", terminate_voltage);
        info!("QMAX: {}", Self::get_qmax(&block));
        info!("update status: {}", update_status);

        if capacity != BATTERY_CAPACITY
            || energy != BATTERY_ENERGY
            || taper_rate != BATTERY_TAPER_RATE
            || terminate_voltage != BATTERY_TERM_V
            || (start_learning && update_status != LEARNING_UPDATE_STATUS)
        {
            warn!("memory block needs update");

            Self::set_capacity(&mut block, BATTERY_CAPACITY);
            Self::set_energy(&mut block, BATTERY_ENERGY);
            Self::set_terminate_voltage(&mut block, BATTERY_TERM_V);
            Self::set_taper_rate(&mut block, BATTERY_TAPER_RATE);
            Self::set_update_status(&mut block, LEARNING_UPDATE_STATUS);

            self.gauge
                .memblock_write(blockdefs::STATE_CLASS, 0, &block)
                .await?;

            info!("memory block updated successfully");
        }

        // self.gauge.soft_reset().await?;
        Ok(())
    }

    fn get_discharge_current_threshold(block: &MemoryBlock) -> u16 {
        Self::read_u16(block, 0)
    }

    fn set_discharge_current_threshold(block: &mut MemoryBlock, v: u16) {
        Self::write_u16(block, 0, v)
    }

    fn get_quit_current_threshold(block: &MemoryBlock) -> u16 {
        Self::read_u16(block, 4)
    }

    fn set_quit_current_threshold(block: &mut MemoryBlock, v: u16) {
        Self::write_u16(block, 4, v)
    }

    async fn set_current_thresholds(&mut self) -> Result<(), bq27xxx::ChipError<E>> {
        let mut block = self
            .gauge
            .memblock_read(blockdefs::CURRENT_THRESHOLDS, 0)
            .await?;

        // all values are relative to battery capacity
        // val = Design capacity / (current * 0.1)
        const DISCHARGE_I_THRESHOLD_VAL: u16 = 1000; // XXX: this is for 5 ma
        const QUIT_I_THRESHOLD_VAL: u16 = 500; // 10 mA

        debug!("block: {}", block.raw);

        let discharge_i_threshold = Self::get_discharge_current_threshold(&block);
        let quit_i_threshold = Self::get_quit_current_threshold(&block);

        info!(
            "discharge current threshold: {} .1 Hr",
            discharge_i_threshold
        );
        info!("quit current threshold: {} .1 Hr", quit_i_threshold);

        if discharge_i_threshold != DISCHARGE_I_THRESHOLD_VAL
            || quit_i_threshold != QUIT_I_THRESHOLD_VAL
        {
            warn!("current limits need an update");

            Self::set_discharge_current_threshold(&mut block, DISCHARGE_I_THRESHOLD_VAL);
            Self::set_quit_current_threshold(&mut block, QUIT_I_THRESHOLD_VAL);

            self.gauge
                .memblock_write(blockdefs::CURRENT_THRESHOLDS, 0, &block)
                .await?;

            info!("memory block updated successfully");
        }

        Ok(())
    }

    async fn probe(&mut self) -> Result<(), bq27xxx::ChipError<E>> {
        self.gauge.probe().await?;

        self.set_battery_state().await?;
        self.set_current_thresholds().await?;

        match self.gauge.read_chem_id().await? {
            ChemId::B4200 => info!("fuelgauge chem id correct"),
            _ => {
                warn!("chem id is not set, applying");
                self.gauge.write_chem_id(ChemId::B4200).await?;
            }
        }

        self.print_stats().await
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
pub async fn run(
    indications: &'static LedIndicationsSignal,
    r: PowerResources,
    i2c: &'static SharedI2cBus,
) {
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
        select(charger.poll(), fuelgauge.wait_int()).await;
    }
}
