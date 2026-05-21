use defmt::bitflags;

use crate::ble::types::{ControlParams, PidParams};

#[derive(Clone)]
pub enum Request {
    PidUpdate(PidParams),
    ControlUpdate(ControlParams),
    Reboot,
    FuelgaugeReset,
    StartScan,
}

bitflags! {
    #[derive(Default)]
    pub struct ButtonFlags:u32 {
        const BUTTON_A = 1 << 0;
        const BUTTON_B = 1 << 1;
        const BUTTON_X = 1 << 3;
        const BUTTON_Y = 1 << 4;
        const BUTTON_LB = 1 << 6;
        const BUTTON_RB = 1 << 7;
        const BUTTON_ACTION_1 = 1 << 10;
        const BUTTON_MENU = 1 << 11;
        const BUTTON_XBOX = 1 << 12;
        const BUTTON_LEFT_STICK = 1 << 13;
        const BUTTON_RIGHT_STICK = 1 << 14;
        const BUTTON_ACTION_2 = 1 << 16;
    }
}

#[derive(defmt::Format, Default, Copy, Clone)]
pub struct JoystickData {
    pub j1: (i32, i32),
    pub j2: (i32, i32),
    pub t1: u16,
    pub t2: u16,
    pub buttons: ButtonFlags,
}
