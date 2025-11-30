use defmt::info;
use embassy_futures::select::select;
use embassy_nrf::{
    gpio::{self, Level, Output, OutputDrive},
    pwm::{self, DutyCycle, SimplePwm},
    saadc::{self, Saadc},
};
use embassy_time::{Duration, Instant, Ticker};
use pid::Pid;

use crate::{
    ble::events::BluetoothEventsProxy,
    power::stats::{PowerStats, UpdateType},
    xbox::JoystickData,
    ControllerResources, Irqs,
};

struct Controller<'a> {
    pwm: SimplePwm<'a>,
    adc: Saadc<'a, 1>,
    gyro_power: gpio::Output<'a>,
    tail_n: gpio::Output<'a>,
    pid: Pid<f32>,
    ps: &'a PowerStats,
}

impl<'a> Controller<'a> {
    const PWM_MAX_DUTY: u16 = 512;
    const RECEIVE_TIMEOUT: Duration = Duration::from_secs(1);
    const CONTROL_LOOP_FREQUENCY_HZ: u64 = 200;

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

        let val = buf[0] + 717;
        val as f32 * 600.0 / (2048.0 * 0.5 * 0.67)
    }

    fn update(&mut self, position: &JoystickData, ang_rate: f32) {
        let throttle = (position.j1.1 >> 6).max(0);
        let yaw = position.j2.0 >> 6;

        let control = if throttle > 50 {
            self.pid.setpoint = -yaw as f32;
            self.pid.next_control_output(ang_rate).output as i32
        } else {
            0
        };

        let rotor1 = throttle + control;
        let rotor2 = throttle - control;
        let elevator = position.j2.1 >> 6;

        self.set_pwm(rotor1, rotor2, elevator);
    }

    async fn run(&mut self, proxy: &BluetoothEventsProxy) {
        let mut receiver = self.ps.event_receiver().unwrap();

        let mut ticker = Ticker::every(Duration::from_hz(Self::CONTROL_LOOP_FREQUENCY_HZ));
        let mut input: JoystickData = Default::default();

        let mut last_receive = Instant::now();

        self.adc.calibrate().await;
        self.gyro_power.set_high();

        loop {
            if let Some(event) = receiver.try_changed() {
                match event {
                    UpdateType::PidUpdate(p, i, d) => {
                        info!("updating pid params: p: {}, i: {}, d: {}", p, i, d);
                        self.pid.p(p, 100.0).i(i, 100.0).d(d, 100.0);
                    }
                    _ => {}
                }
            }

            // Just take the latest value if any
            if let Some(data) = proxy.joystick_data_take() {
                input = data;
                last_receive = Instant::now();
            }

            if Instant::now().duration_since(last_receive) > Self::RECEIVE_TIMEOUT {
                input = Default::default();
            }

            let speed = self.read_angular_speed().await;
            self.update(&input, speed);

            // XXX: This might be too fast...
            // self.ps.add_gyro_sample(speed as i16);

            ticker.next().await;
        }
    }

    fn new(r: &'a mut ControllerResources, ps: &'a PowerStats) -> Self {
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

        let gyro_power = Output::new(r.gyro_power.reborrow(), Level::Low, OutputDrive::Standard);
        let tail_n = Output::new(r.tail_n.reborrow(), Level::Low, OutputDrive::Standard);

        let mut pid = Pid::new(0.0, 1000.0);
        pid.p(2.0, 1000.0).i(0.0, 1000.0).d(0.0, 1000.0);

        Self {
            adc,
            gyro_power,
            pwm,
            tail_n,
            pid,
            ps,
        }
    }
}

#[embassy_executor::task]
pub async fn run(
    proxy: &'static BluetoothEventsProxy,
    ps: &'static PowerStats,
    mut r: ControllerResources,
) {
    loop {
        let mut run_controller = async |proxy| {
            let mut c = Controller::new(&mut r, ps);
            c.run(proxy).await;
        };

        proxy.wait_connect().await;
        select(run_controller(proxy), proxy.wait_disconnect()).await;
    }
}
