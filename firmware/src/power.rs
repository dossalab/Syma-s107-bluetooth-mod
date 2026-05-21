use crate::{
    ble::types::{ChargerState, OcvMeasurement, PeriodicUpdate, QmaxUpdate, RaTableUpdate},
    state::{StateReceiver, StateSender, SystemState},
    types::Request,
    PowerResources, SharedI2cBus, SharedI2cDevice,
};
use bq27xxx::{
    chips::bq27427::{ChemInfo, CurrentThresholds, RaTable, StateClass},
    defs::{ControlStatusFlags, StatusFlags},
    memory::MemoryBlock,
    Bq27xx, ChemId,
};
use defmt::{debug, error, info, unwrap};
use embassy_embedded_hal::shared_bus::{asynch::i2c::I2cDevice, I2cDeviceError};
use embassy_futures::{
    join::join,
    select::{select, select4, Either4},
};
use embassy_nrf::{
    gpio::{Input, Pull},
    twim,
};
use embassy_time::{Instant, Timer};

type GaugeResult<T> = Result<T, bq27xxx::ChipError<I2cDeviceError<twim::Error>>>;

enum GaugeRefreshReason {
    Periodic,
    Interrupt,
}

enum GaugeState {
    Initializing { from_reset: bool },
    Idle,
}

struct Gauge<'a> {
    gauge: Bq27xx<SharedI2cDevice<'a>, embassy_time::Delay>,
    int: Input<'a>,

    state: GaugeState,
    high_speed_refresh: bool,

    // State receivers and senders
    controller_connected_receiver: StateReceiver<'a, bool>,
    soc_sender: StateSender<'a, u8>,
    periodic_update_sender: StateSender<'a, PeriodicUpdate>,
    requests_receiver: StateReceiver<'a, Request>,
    ocv_taken_sender: StateSender<'a, OcvMeasurement>,
    qmax_update_sender: StateSender<'a, QmaxUpdate>,
    ratable_update_sender: StateSender<'a, RaTableUpdate>,

    // Flags tracker
    prev_flags: (StatusFlags, ControlStatusFlags),
}

impl<'a> Gauge<'a> {
    const GAUGE_I2C_ADDR: u8 = 0x55;

    async fn configure_gauge(&mut self) -> GaugeResult<()> {
        self.gauge.probe().await?;
        self.gauge.write_chem_id(ChemId::B4200).await?;

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
            .await?;

        self.gauge
            .memory_modify(|b: &mut ChemInfo| {
                b.set_v_taper(4200); // mV
            })
            .await?;

        // Read back the values to confirm
        info!("state: {}", self.gauge.memblock_read::<StateClass>().await?);
        info!(
            "ratable: {}",
            self.gauge.memblock_read::<RaTable>().await?.as_bytes()
        );

        info!("chem: {}", self.gauge.memblock_read::<ChemInfo>().await?);

        Ok(())
    }

    async fn track_flags(&mut self) -> GaugeResult<(StatusFlags, ControlStatusFlags)> {
        let flags = self.gauge.get_flags().await?;
        let control = self.gauge.get_control_status().await?;

        let new_flags = flags - self.prev_flags.0;
        let new_control = control - self.prev_flags.1;

        self.prev_flags = (flags, control);

        Ok((new_flags, new_control))
    }

    async fn refresh(&mut self, wakeup_reason: GaugeRefreshReason) -> GaugeResult<()> {
        let (flags, control_flags) = self.track_flags().await?;

        if !flags.is_empty() || !control_flags.is_empty() {
            info!("flags: {}, control {}", flags, control_flags);
        }

        match self.state {
            GaugeState::Initializing { ref mut from_reset } => {
                debug!("initialization state");

                if flags.contains(StatusFlags::ITPOR) {
                    *from_reset = true;
                }

                if control_flags.contains(ControlStatusFlags::INITCOMP) {
                    if *from_reset {
                        info!("init complete, configuring gauge");
                        self.configure_gauge().await?;
                    } else {
                        info!("init complete, gauge was already running");
                    }
                    self.state = GaugeState::Idle;

                    let soc = self.gauge.state_of_charge().await?;
                    info!("initial soc - {}%", soc);
                    self.soc_sender.send(soc as u8);
                }
            }

            GaugeState::Idle => {
                debug!("idle state");

                // let's check if we're coming out of reset...
                if flags.contains(StatusFlags::ITPOR) {
                    info!("power on reset detected");
                    self.state = GaugeState::Initializing { from_reset: true };

                    // There is no point to talk to the gauge at this stage
                    return Ok(());
                }

                if flags.contains(StatusFlags::OCVTAKEN) {
                    info!("OCV measurement taken");
                    self.ocv_taken_sender.send(OcvMeasurement {
                        timestamp: Instant::now().into(),
                    });
                }

                if control_flags.contains(ControlStatusFlags::RES_UP) {
                    info!("Ra table updated");
                    self.ratable_update_sender.send(RaTableUpdate {
                        timestamp: Instant::now().into(),
                    });
                }

                if control_flags.contains(ControlStatusFlags::QMAX_UP) {
                    // Let's also read and report the current QMax value
                    let state = self.gauge.memblock_read::<StateClass>().await?;

                    info!("QMax updated (new value {})", state.get_qmax());
                    self.qmax_update_sender.send(QmaxUpdate {
                        value: state.get_qmax(),
                        timestamp: Instant::now().into(),
                    });
                }

                match wakeup_reason {
                    GaugeRefreshReason::Periodic => {
                        let voltage = self.gauge.voltage().await?;
                        let current = self.gauge.average_current().await?;
                        let temperature = self.gauge.temperature().await?;

                        info!(
                            "periodic - {} mV, {} mA, {} .K",
                            voltage, current, temperature
                        );

                        self.periodic_update_sender.send(PeriodicUpdate {
                            voltage,
                            current,
                            temperature,
                        });
                    }

                    GaugeRefreshReason::Interrupt => {
                        let soc = self.gauge.state_of_charge().await?;

                        info!("soc - {}%", soc);
                        self.soc_sender.send(soc as u8);
                    }
                }
            }
        }

        Ok(())
    }

    async fn run(&mut self) -> ! {
        if let Err(err) = self.refresh(GaugeRefreshReason::Periodic).await {
            error!("error - {}", err);
        }

        loop {
            let periodic_refresh = async || {
                if self.high_speed_refresh {
                    Timer::after_secs(1).await
                } else {
                    Timer::after_secs(30).await
                }
            };

            let s = select4(
                self.int.wait_for_falling_edge(),
                periodic_refresh(),
                self.requests_receiver.changed(),
                self.controller_connected_receiver.changed(),
            )
            .await;

            let res = match s {
                Either4::First(_) => self.refresh(GaugeRefreshReason::Interrupt).await,
                Either4::Second(_) => self.refresh(GaugeRefreshReason::Periodic).await,

                Either4::Third(Request::FuelgaugeReset) => {
                    info!("performing gauge reset");

                    let r1 = self.gauge.reset().await;
                    self.state = GaugeState::Initializing { from_reset: true };
                    let r2 = self.refresh(GaugeRefreshReason::Periodic).await;

                    r1.and(r2)
                }

                Either4::Fourth(connected) => {
                    self.high_speed_refresh = connected;

                    info!("periodic refresh: {}", self.high_speed_refresh);
                    Ok(())
                }

                _ => Ok(()),
            };

            if let Err(err) = res {
                error!("error - {}", err);
            }
        }
    }

    fn new(ss: &'a SystemState, i2c_dev: SharedI2cDevice<'a>, int: Input<'a>) -> Self {
        Self {
            gauge: Bq27xx::new(i2c_dev, embassy_time::Delay, Self::GAUGE_I2C_ADDR),
            int,

            state: GaugeState::Initializing { from_reset: false },
            high_speed_refresh: false,

            soc_sender: ss.soc.sender(),
            periodic_update_sender: ss.periodic_update.sender(),
            requests_receiver: unwrap!(ss.requests.receiver()),
            controller_connected_receiver: unwrap!(ss.controller_connected.receiver()),
            ocv_taken_sender: ss.ocv_measurement.sender(),
            qmax_update_sender: ss.qmax_update.sender(),
            ratable_update_sender: ss.ratable_update.sender(),

            prev_flags: (StatusFlags::empty(), ControlStatusFlags::empty()),
        }
    }
}

struct Charger<'a> {
    charger_state_sender: StateSender<'a, ChargerState>,
    fault_int: Input<'a>,
    charging_int: Input<'a>,
}

impl<'a> Charger<'a> {
    async fn run(&mut self) -> ! {
        loop {
            let failure = self.fault_int.is_low();
            let charging = self.charging_int.is_low();

            info!(
                "charger status: failure: {}, charging: {}",
                failure, charging
            );

            self.charger_state_sender
                .send(ChargerState { failure, charging });

            select(
                self.fault_int.wait_for_any_edge(),
                self.charging_int.wait_for_any_edge(),
            )
            .await;
        }
    }

    fn new(ss: &'a SystemState, fault_int: Input<'a>, charging_int: Input<'a>) -> Self {
        Self {
            fault_int,
            charging_int,
            charger_state_sender: ss.charger_state.sender(),
        }
    }
}

#[embassy_executor::task]
pub async fn run(state: &'static SystemState, r: PowerResources, i2c: &'static SharedI2cBus) {
    info!("running power task");

    let mut gauge = Gauge::new(
        state,
        I2cDevice::new(i2c),
        Input::new(r.fuelgauge_int, Pull::Up),
    );

    let mut charger = Charger::new(
        state,
        Input::new(r.fault_int, Pull::Up),
        Input::new(r.charging_int, Pull::Up),
    );

    join(charger.run(), gauge.run()).await;
}
