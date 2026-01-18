use defmt::{info, unwrap};
use embassy_futures::select::{select, Either};
use embassy_nrf::{
    gpio::{self, Level, Output, OutputDrive},
    pwm::{self, DutyCycle, SimplePwm},
    saadc::{self, Saadc},
};
use embassy_time::{Duration, Ticker, Timer};
use pid::Pid;

use crate::{
    state::{SystemState, UpdateType},
    xbox::JoystickData,
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
}

impl<'a> Controller<'a> {
    const PWM_MAX_DUTY: u16 = 512;
    const PID_CONTROL_LIMIT: u16 = Self::PWM_MAX_DUTY / 2;
    const RECEIVE_TIMEOUT: Duration = Duration::from_secs(1);

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
        let yaw = self.input.j2.0 >> 6;

        let control = if throttle > 10 {
            let ang_rate = self.read_angular_speed().await;

            info!("ang rate: {}", ang_rate);

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

    fn set_pid(&mut self, p: f32, i: f32, d: f32) {
        self.pid
            .p(p, Self::PID_CONTROL_LIMIT)
            .i(i, Self::PID_CONTROL_LIMIT)
            .d(d, Self::PID_CONTROL_LIMIT);
    }

    async fn init(r: &'a mut ControllerResources) -> Self {
        let mut pwm_config = pwm::SimpleConfig::default();

        pwm_config.max_duty = Controller::PWM_MAX_DUTY;
        pwm_config.prescaler = pwm::Prescaler::Div16;

        let mut adc_config = saadc::Config::default();

        adc_config.resolution = saadc::Resolution::_12BIT;
        adc_config.oversample = saadc::Oversample::OVER4X;

        let mut adc_channel_config =
            saadc::ChannelConfig::differential(r.gyro_input.reborrow(), r.gyro_vref.reborrow());

        // Some considerations here:
        // - gyro vref is 1.35v, our ADC vref is 600 mV;
        // - 0.67 mV per deg/s;
        // - maximum angular velocity is 300 deg/s, which is ~200 mV;
        // - however, some natural DC offset seem to be taking place, so we need wider range

        adc_channel_config.time = saadc::Time::_40US;
        adc_channel_config.gain = saadc::Gain::GAIN1_2;

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

        let mut pid = Pid::new(0.0, Self::PWM_MAX_DUTY);
        pid.p(0.3, Self::PID_CONTROL_LIMIT)
            .i(0.0, Self::PID_CONTROL_LIMIT)
            .d(0.0, Self::PID_CONTROL_LIMIT);

        adc.calibrate().await;

        // Give gyro some time to settle
        Timer::after_millis(50).await;

        Self {
            adc,
            _gyro_power: gyro_power,
            pwm,
            tail_n,
            pid,
            input: Default::default(),
            gyro_offset: 742,
        }
    }
}

#[embassy_executor::task]
pub async fn run(state: &'static SystemState, mut r: ControllerResources) {
    let mut receiver = unwrap!(state.event_receiver());

    let run_controller = async || {
        info!("running controller");

        const CONTROL_LOOP_RATE: Duration = Duration::from_hz(200);

        let mut controller = Controller::init(&mut r).await;
        let mut ticker = Ticker::every(CONTROL_LOOP_RATE);

        loop {
            let r = select(receiver.changed(), ticker.next()).await;
            match r {
                Either::First(event) => match event {
                    UpdateType::PidUpdate(p, i, d) => {
                        info!("updating pid params: p: {}, i: {}, d: {}", p, i, d);
                        controller.set_pid(p, i, d);
                    }

                    UpdateType::ControllerData(jd) => controller.add_input(jd),
                    _ => {}
                },

                Either::Second(_) => controller.tick().await,
            }
        }
    };

    let predicate =
        || !state.is_charging() && !state.is_soc_fatal() && state.is_controller_connected();

    state.run_while(predicate, run_controller).await;
}
