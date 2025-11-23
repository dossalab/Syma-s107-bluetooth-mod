use embassy_futures::{
    join::join,
    select::{select, Either},
};
use embassy_nrf::{
    gpio::{self, Level, Output, OutputDrive},
    pwm::{self, DutyCycle, SimplePwm},
    saadc,
};
use embassy_time::{Duration, Ticker, Timer};

use crate::{
    ble::events::BluetoothEventsProxy, power::stats::PowerStats, xbox::JoystickData, GyroResources,
    Irqs, MotorResources,
};

struct Controller<'a> {
    pwm: SimplePwm<'a>,
    tail_n: gpio::Output<'a>,
}

impl<'a> Controller<'a> {
    const RUDDER_SCALE: i32 = 4;
    const PWM_MAX_DUTY: u16 = 512;

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

    fn update(&mut self, position: &JoystickData) {
        let throttle = (position.j1.1 >> 6).max(0);
        let yaw = position.j2.0 >> 6;

        let rotor1 = throttle - (yaw / Self::RUDDER_SCALE);
        let rotor2 = throttle + (yaw / Self::RUDDER_SCALE);
        let elevator = position.j2.1 >> 6;

        self.set_pwm(rotor1, rotor2, elevator);
    }

    fn reset(&mut self) {
        self.set_pwm(0, 0, 0);
    }

    fn new(pwm: SimplePwm<'a>, tail_n: Output<'a>) -> Self {
        Self { pwm, tail_n }
    }
}

#[embassy_executor::task]
pub async fn run(
    proxy: &'static BluetoothEventsProxy,
    ps: &'static PowerStats,
    mut mr: MotorResources,
    mut gr: GyroResources,
) {
    const RECEIVE_TIMEOUT: Duration = Duration::from_secs(1);

    loop {
        let run_controller = async {
            let mut config = pwm::SimpleConfig::default();

            config.max_duty = Controller::PWM_MAX_DUTY;
            config.prescaler = pwm::Prescaler::Div16;

            let pwm = SimplePwm::new_3ch(
                mr.pwm.reborrow(),
                // Recheck channel id assignments above if changing order
                mr.rotor1.reborrow(),
                mr.rotor2.reborrow(),
                mr.tail_p.reborrow(),
                &config,
            );

            let tail_n = Output::new(mr.tail_n.reborrow(), Level::Low, OutputDrive::Standard);
            let mut c = Controller::new(pwm, tail_n);

            loop {
                let r = select(proxy.wait_joystick_data(), Timer::after(RECEIVE_TIMEOUT)).await;
                match r {
                    Either::First(position) => c.update(&position),
                    Either::Second(_) => c.reset(),
                }
            }
        };

        let run_adc_loop = async {
            let mut config = saadc::Config::default();

            config.resolution = saadc::Resolution::_12BIT;

            let mut channel_config =
                saadc::ChannelConfig::differential(gr.input.reborrow(), gr.vref.reborrow());

            // Some considerations here:
            // - gyro vref is 1.35v, our ADC vref is 600 mV;
            // - 0.67 mV per deg/s;
            // - maximum angular velocity is 300 deg/s, which is ~200 mV;
            // - however, some natural DC offset seem to be taking place
            //
            // ADC equations are:
            // Vdiff (volts) = reading * 0.6 / (gain * 2^resolution-1) = reading * 0.6 / 2048
            // speed = Vdiff (volts) * 1000 / 0.67 = Vdiff * 600 / (2048 * 0.67)

            channel_config.time = saadc::Time::_40US;
            channel_config.gain = saadc::Gain::GAIN1_2;

            let mut adc = saadc::Saadc::new(gr.adc.reborrow(), Irqs, config, [channel_config]);
            adc.calibrate().await;

            // Power up the gyro
            let _power = Output::new(gr.power.reborrow(), Level::High, OutputDrive::Standard);

            let mut ticker = Ticker::every(Duration::from_millis(50));
            let dc_off = -725;

            loop {
                let mut buf = [0; 1];

                adc.sample(&mut buf).await;

                let val = buf[0] - dc_off;
                let speed = val as f32 * 600.0 / (2048.0 * 0.5 * 0.67);

                ps.add_gyro_sample(speed as i16);
                ticker.next().await;
            }
        };

        proxy.wait_connect().await;
        select(join(run_controller, run_adc_loop), proxy.wait_disconnect()).await;
    }
}
