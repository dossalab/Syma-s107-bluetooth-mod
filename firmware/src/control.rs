use defmt::{info, unwrap};
use embassy_futures::select::{select3, Either3};
use embassy_nrf::{
    gpio::{self, Level, Output, OutputDrive},
    pwm::{self, DutyCycle, SimplePwm},
    saadc::{self, Saadc},
};
use embassy_time::{Duration, Ticker, Timer};
use pid::Pid;

use crate::{
    ble::types::Configuration, state::SystemState, types::JoystickData, utils, xbox,
    ControllerResources, Irqs,
};

struct Controller<'a> {
    pwm: SimplePwm<'a>,
    adc: Saadc<'a, 1>,
    _gyro_power: gpio::Output<'a>,
    tail_n: gpio::Output<'a>,
    pid: Pid<f32>,
    input: JoystickData,
    gyro_offset: i32,
    yaw_expo: f32,
}

/// Applies an expo (exponential) curve to a joystick axis value.
///
/// `expo` in [0.0, 1.0]: 0 = fully linear, 1 = fully cubic.
/// The curve blends linear and cubic response, which softens sensitivity
/// near center while preserving full deflection at the edges.
fn apply_expo(value: i32, max: i32, expo: f32) -> i32 {
    let n = value as f32 / max as f32;
    let curved = n * ((1.0 - expo) + expo * n * n);
    (curved * max as f32) as i32
}

impl<'a> Controller<'a> {
    const PWM_MAX_DUTY: u16 = 512;
    const PID_CONTROL_LIMIT: u16 = Self::PWM_MAX_DUTY / 2;
    const RECEIVE_TIMEOUT: Duration = Duration::from_secs(1);
    /// Max yaw axis value after the >> 6 shift applied in tick().
    const YAW_STICK_MAX: i32 = xbox::STICKS_RANGE / 2 >> 6;

    fn set_pwm(&mut self, r1: i32, r2: i32, v: i32) {
        let clamp_to_pwm = |x: i32| x.clamp(0, Self::PWM_MAX_DUTY as i32) as u16;

        let tail = if v > 0 {
            self.tail_n.set_high();

            Self::PWM_MAX_DUTY as i32 - v
        } else {
            self.tail_n.set_low();
            -v
        };

        let duties = [
            DutyCycle::inverted(clamp_to_pwm(r1)),
            DutyCycle::inverted(clamp_to_pwm(r2)),
            DutyCycle::inverted(clamp_to_pwm(tail)),
            DutyCycle::inverted(0), // unused
        ];

        self.pwm.set_all_duties(duties);
    }

    async fn read_angular_speed(&mut self) -> f32 {
        let mut buf = [0; 1];

        self.adc.sample(&mut buf).await;

        // ADC equations are:
        // Vdiff (volts) = reading * 0.6 / (gain * 2^resolution-1) = reading * 0.6 / 2048
        // speed = Vdiff (volts) * 1000 / 0.67 = Vdiff * 600 / (2048 * 0.67)

        let val = buf[0] as i32 + self.gyro_offset;
        val as f32 * 600.0 / (2048.0 * 0.5 * 0.67)
    }

    async fn tick(&mut self) {
        let throttle = (self.input.j1.1 >> 6).max(0);
        let yaw_raw = self.input.j2.0 >> 6;
        let yaw = apply_expo(yaw_raw, Self::YAW_STICK_MAX, self.yaw_expo);

        let control = if throttle > 10 {
            let ang_rate = self.read_angular_speed().await;

            self.pid.setpoint = -yaw as f32;
            self.pid.next_control_output(ang_rate).output as i32
        } else {
            0
        };

        let rotor1 = throttle + control;
        let rotor2 = throttle - control;
        let elevator = self.input.j2.1 >> 6;

        self.set_pwm(rotor1, rotor2, elevator);
    }

    fn add_input(&mut self, jd: JoystickData) {
        self.input = jd;
    }

    fn apply_config(&mut self, config: Configuration) {
        self.pid
            .p(config.p, Self::PID_CONTROL_LIMIT)
            .i(config.i, Self::PID_CONTROL_LIMIT)
            .d(config.d, Self::PID_CONTROL_LIMIT);
        self.yaw_expo = config.yaw_expo.clamp(0.0, 1.0);
    }

    async fn init(r: &'a mut ControllerResources, config: Configuration) -> Self {
        let mut pwm_config = pwm::SimpleConfig::default();

        pwm_config.max_duty = Controller::PWM_MAX_DUTY;
        pwm_config.prescaler = pwm::Prescaler::Div16;

        let mut adc_config = saadc::Config::default();

        adc_config.resolution = saadc::Resolution::_12bit;
        adc_config.oversample = saadc::Oversample::Over4x;

        let mut adc_channel_config =
            saadc::ChannelConfig::differential(r.gyro_input.reborrow(), r.gyro_vref.reborrow());

        // Some considerations here:
        // - gyro vref is 1.35v, our ADC vref is 600 mV;
        // - 0.67 mV per deg/s;
        // - maximum angular velocity is 300 deg/s, which is ~200 mV;
        // - however, some natural DC offset seem to be taking place, so we need wider range

        adc_channel_config.time = saadc::Time::_40US;
        adc_channel_config.gain = saadc::Gain::Gain1_2;

        let pwm = SimplePwm::new_3ch(
            r.pwm.reborrow(),
            // Recheck channel id assignments above if changing order
            r.rotor1.reborrow(),
            r.rotor2.reborrow(),
            r.tail_p.reborrow(),
            &pwm_config,
        );

        let adc = saadc::Saadc::new(r.adc.reborrow(), Irqs, adc_config, [adc_channel_config]);
        let gyro_power = Output::new(r.gyro_power.reborrow(), Level::High, OutputDrive::Standard);
        let tail_n = Output::new(r.tail_n.reborrow(), Level::Low, OutputDrive::Standard);

        adc.calibrate().await;

        // Give gyro some time to settle
        Timer::after_millis(50).await;

        let mut s = Self {
            adc,
            _gyro_power: gyro_power,
            pwm,
            tail_n,
            pid: Pid::new(0.0, Self::PWM_MAX_DUTY),
            input: Default::default(),
            gyro_offset: 742,
            yaw_expo: 0.0,
        };

        s.apply_config(config);
        s
    }
}

#[embassy_executor::task]
pub async fn run(state: &'static SystemState, mut r: ControllerResources) {
    let mut config_receiver = unwrap!(state.config.receiver());
    let mut controller_sample_receiver = unwrap!(state.controller_sample.receiver());
    let controller_run_allowed_receiver = unwrap!(state.controller_run_allowed.receiver());

    let run_controller = async || {
        info!("running controller");

        const CONTROL_LOOP_RATE: Duration = Duration::from_hz(200);

        let initial_config = config_receiver.try_get().unwrap_or_default();
        let mut controller = Controller::init(&mut r, initial_config).await;
        let mut ticker = Ticker::every(CONTROL_LOOP_RATE);

        loop {
            match select3(
                config_receiver.changed(),
                controller_sample_receiver.changed(),
                ticker.next(),
            )
            .await
            {
                Either3::First(config) => controller.apply_config(config),
                Either3::Second(input) => controller.add_input(input),
                Either3::Third(_) => controller.tick().await,
            }
        }
    };

    utils::run_with_receiver(controller_run_allowed_receiver, run_controller).await;
}
