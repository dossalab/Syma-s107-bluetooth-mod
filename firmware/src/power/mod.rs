use core::future;

use crate::{power::stats::PeriodicUpdate, PowerResources, SharedI2cBus};
use bq27xxx::{
    chips::bq27427::{ChemInfo, CurrentThresholds, RaTable, StateClass},
    defs::{ControlStatusFlags, StatusFlags},
    memory::{self, MemoryBlock},
    Bq27xx, ChemId,
};
use defmt::{error, info};
use embassy_embedded_hal::shared_bus::{asynch::i2c::I2cDevice, I2cDeviceError};
use embassy_futures::select::{select, Either};
use embassy_nrf::{
    gpio::{Input, Pull},
    twim,
};
use embassy_sync::blocking_mutex::raw::NoopRawMutex;
use embassy_time::{Duration, Timer};
use stats::PowerStats;

pub mod stats;

type Gauge<'a> = Bq27xx<I2cDevice<'a, NoopRawMutex, twim::Twim<'a>>, embassy_time::Delay>;
type GaugeResult<T> = Result<T, bq27xxx::ChipError<I2cDeviceError<twim::Error>>>;

async fn wait_gauge_init_complete<'a>(gauge: &mut Gauge<'a>) -> GaugeResult<()> {
    for _ in 0..10 {
        let control_flags = gauge.get_control_status().await?;

        if control_flags.contains(ControlStatusFlags::INITCOMP) {
            info!("fuelgauge init complete!");
            return Ok(());
        }

        Timer::after_secs(1).await;
    }

    Err(bq27xxx::ChipError::PollTimeout)
}

async fn configure_gauge<'a>(gauge: &mut Gauge<'a>) -> GaugeResult<()> {
    gauge.write_chem_id(ChemId::B4200).await?;

    let start_learning = false;

    info!("updating fuelgauge memory...");

    gauge
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

    gauge
        .memory_modify(|b: &mut CurrentThresholds| {
            b.set_discharge_current_threshold(400);
            b.set_quit_current_threshold(200);
        })
        .await?;

    gauge
        .memory_modify(|b: &mut RaTable| {
            // This is obtained from learning cycle :)
            b.set_points([50, 30, 34, 46, 38, 32, 37, 31, 32, 35, 39, 39, 61, 115, 200]);
        })
        .await?;

    gauge
        .memory_modify(|b: &mut ChemInfo| {
            b.set_v_taper(4200); // mV
        })
        .await
}

#[embassy_executor::task]
pub async fn run(stats: &'static PowerStats, mut r: PowerResources, i2c: &'static SharedI2cBus) {
    const GAUGE_I2C_ADDR: u8 = 0x55;
    const GAUGE_INIT_RETRY_INTERVAL: Duration = Duration::from_secs(10);
    const GAUGE_PERIODIC_POLL_INTERVAL: Duration = Duration::from_secs(1);

    let force_memory_update = false;

    info!("running power task");

    let mut poll_gauge = async |do_periodic: bool| -> GaugeResult<()> {
        let dev = I2cDevice::new(i2c);

        let mut int = Input::new(r.fuelgauge_int.reborrow(), Pull::Up);
        let mut gauge = Bq27xx::new(dev, embassy_time::Delay, GAUGE_I2C_ADDR);

        gauge.probe().await?;

        let flags = gauge.get_flags().await?;

        if flags.contains(StatusFlags::ITPOR) || force_memory_update {
            info!("fuelgauge ITPOR condition");

            wait_gauge_init_complete(&mut gauge).await?;
            configure_gauge(&mut gauge).await?;
        } else {
            // still dump the info for debugging
            info!("state: {}", gauge.memblock_read::<StateClass>().await?);
            info!(
                "ratable: {}",
                gauge.memblock_read::<RaTable>().await?.as_bytes()
            );

            info!("chem: {}", gauge.memblock_read::<ChemInfo>().await?);
        }

        let next_periodic_update = async || match do_periodic {
            true => Timer::after(GAUGE_PERIODIC_POLL_INTERVAL).await,
            false => future::pending().await,
        };

        // SoC is important for internal decisions, so poll it once to see where we stand.
        // Other stats will be gathered as we go
        stats.add_soc(gauge.state_of_charge().await? as u8);

        loop {
            let r = select(int.wait_for_low(), next_periodic_update()).await;
            match r {
                Either::First(_) => {
                    info!("fuelgauge interrupt");
                    stats.add_soc(gauge.state_of_charge().await? as u8);
                }
                Either::Second(_) => {
                    info!("fuelgauge periodic update");

                    let u = PeriodicUpdate {
                        voltage: gauge.voltage().await?,
                        current: gauge.average_current().await?,
                        temperature: gauge.temperature().await?,
                    };

                    stats.add_periodic_update(u);
                }
            }
        }
    };

    let mut poll_charger = async || {
        let mut fault = Input::new(r.fault_int.reborrow(), Pull::Up);
        let mut charging = Input::new(r.charging_int.reborrow(), Pull::Up);

        stats.set_charging(charging.is_low());
        stats.set_charger_failure(fault.is_low());

        loop {
            let r = select(charging.wait_for_any_edge(), fault.wait_for_any_edge()).await;

            info!("charger status update");

            match r {
                Either::First(_) => stats.set_charging(charging.is_low()),
                Either::Second(_) => stats.set_charger_failure(fault.is_low()),
            };
        }
    };

    loop {
        let periodic_update = true;

        match select(poll_gauge(periodic_update), poll_charger()).await {
            Either::First(Err(e)) => {
                error!("gauge initialization failure - {}", e);
                Timer::after(GAUGE_INIT_RETRY_INTERVAL).await
            }

            _ => {}
        }
    }
}
