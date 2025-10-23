use embassy_futures::select::{select, Either};
use embassy_nrf::{
    gpio::{self, Level, Output, OutputDrive},
    pwm::SimplePwm,
};
use embassy_time::{Duration, Timer};

use crate::{ble_events::BluetoothEventsProxy, xbox::JoystickData, MotorResources};

const CHANNEL_ID_ROTOR1: usize = 0;
const CHANNEL_ID_ROTOR2: usize = 1;
const CHANNEL_ID_TAIL: usize = 2;

struct Controller<'a> {
    pwm: SimplePwm<'a>,
    tail_n: gpio::Output<'a>,
}

impl<'a> Controller<'a> {
    const RUDDER_SCALE: i32 = 4;
    const PWM_MAX_DUTY: u16 = 1024;

    fn set_pwm(&mut self, r1: i32, r2: i32, v: i32) {
        let clamp_to_pwm = |x: i32| x.clamp(0, Self::PWM_MAX_DUTY as i32) as u16;

        let value = if v > 0 {
            self.tail_n.set_high();

            Self::PWM_MAX_DUTY as i32 - v
        } else {
            self.tail_n.set_low();
            -v
        };

        self.pwm.set_duty(CHANNEL_ID_ROTOR1, clamp_to_pwm(r1));
        self.pwm.set_duty(CHANNEL_ID_ROTOR2, clamp_to_pwm(r2));
        self.pwm.set_duty(CHANNEL_ID_TAIL, clamp_to_pwm(value));
    }

    fn update(&mut self, position: &JoystickData) {
        let throttle = (position.j1.1 >> 5).max(0);
        let yaw = position.j2.0 >> 5;

        let rotor1 = throttle - (yaw / Self::RUDDER_SCALE);
        let rotor2 = throttle + (yaw / Self::RUDDER_SCALE);
        let elevator = position.j2.1 >> 5;

        self.set_pwm(rotor1, rotor2, elevator);
    }

    fn reset(&mut self) {
        self.set_pwm(0, 0, 0);
    }

    fn new(pwm: SimplePwm<'a>, tail_n: Output<'a>) -> Self {
        pwm.set_max_duty(Self::PWM_MAX_DUTY);
        Self { pwm, tail_n }
    }
}

#[embassy_executor::task]
pub async fn run(proxy: &'static BluetoothEventsProxy, mut r: MotorResources) {
    const RECEIVE_TIMEOUT: Duration = Duration::from_secs(1);

    loop {
        let run_controller = async {
            let pwm = SimplePwm::new_3ch(
                r.pwm.reborrow(),
                // Recheck channel id assignments above if changing order
                r.rotor1.reborrow(),
                r.rotor2.reborrow(),
                r.tail_p.reborrow(),
            );

            let tail_n = Output::new(r.tail_n.reborrow(), Level::Low, OutputDrive::Standard);
            let mut c = Controller::new(pwm, tail_n);

            loop {
                let r = select(proxy.wait_joystick_data(), Timer::after(RECEIVE_TIMEOUT)).await;
                match r {
                    Either::First(position) => c.update(&position),
                    Either::Second(_) => c.reset(),
                }
            }
        };

        proxy.wait_connect().await;
        select(run_controller, proxy.wait_disconnect()).await;
    }
}
