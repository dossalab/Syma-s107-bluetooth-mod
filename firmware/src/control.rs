use embassy_futures::select::{select, Either};
use embassy_nrf::{
    gpio::{self, Level, Output, OutputDrive},
    pwm::{self, SimplePwm},
};
use embassy_time::{Duration, Timer};

use crate::{
    xbox::{JoystickData, JoystickDataSignal},
    MotorResources,
};

struct Controller<'a> {
    pwm: SimplePwm<'a>,
    tail_n: gpio::Output<'a>,
}

impl<'a> Controller<'a> {
    const RUDDER_SCALE: i32 = 4;
    const PWM_MAX_DUTY: u16 = 1024;

    const CHANNEL_ID_ROTOR1: usize = 0;
    const CHANNEL_ID_ROTOR2: usize = 1;
    const CHANNEL_ID_TAIL: usize = 2;

    pub fn new(pwm: SimplePwm<'a>, tail_n: Output<'a>) -> Self {
        pwm.set_max_duty(Self::PWM_MAX_DUTY);

        Self { pwm, tail_n }
    }

    fn set_rotor1(&mut self, v: u16) {
        self.pwm.set_duty(Self::CHANNEL_ID_ROTOR1, v);
    }

    fn set_rotor2(&mut self, v: u16) {
        self.pwm.set_duty(Self::CHANNEL_ID_ROTOR2, v);
    }

    fn set_tail(&mut self, v: i32) {
        let value = if v > 0 {
            self.tail_n.set_high();

            Self::PWM_MAX_DUTY as i32 - v
        } else {
            self.tail_n.set_low();
            -v
        };

        self.pwm
            .set_duty(Self::CHANNEL_ID_TAIL, Self::clamp_to_pwm_range(value));
    }

    fn clamp_to_pwm_range(x: i32) -> u16 {
        return x.clamp(0, Self::PWM_MAX_DUTY as i32) as u16;
    }

    fn reset(&mut self) {
        self.set_rotor1(0);
        self.set_rotor2(0);
        self.set_tail(0);
    }

    fn update(&mut self, position: &JoystickData) {
        let throttle = (position.j1.1 >> 5).clamp(0, 1023);
        let yaw = position.j2.0 >> 5;
        let elevator = position.j2.1 >> 5;

        let rotor1 = Self::clamp_to_pwm_range(throttle - (yaw / Self::RUDDER_SCALE));
        let rotor2 = Self::clamp_to_pwm_range(throttle + (yaw / Self::RUDDER_SCALE));

        self.set_rotor1(rotor1);
        self.set_rotor2(rotor2);
        self.set_tail(elevator);
    }
}

#[embassy_executor::task]
pub async fn run(input: &'static JoystickDataSignal, r: MotorResources) {
    let receive_timeout = Duration::from_secs(1);

    let pwm = SimplePwm::new_3ch(r.pwm, r.rotor1, r.rotor2, r.tail_p);
    let tail_n = Output::new(r.tail_n, Level::Low, OutputDrive::Standard);

    let mut c = Controller::new(pwm, tail_n);

    c.reset();

    loop {
        let r = select(input.wait(), Timer::after(receive_timeout)).await;
        match r {
            Either::First(position) => {
                c.update(&position);
            }
            Either::Second(_) => {
                c.reset();
            }
        }
    }
}
