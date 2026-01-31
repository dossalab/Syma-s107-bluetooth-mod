// Use simple C-style packing to help with BLE serialization

use defmt::bitflags;

#[repr(C, packed)]
#[derive(Default, Copy, Clone)]
pub struct PeriodicUpdate {
    pub voltage: u16,
    pub current: i16,
    pub temperature: u16,
}

#[repr(C, packed)]
#[derive(Default, Copy, Clone)]
pub struct ChargerState {
    pub charging: bool,
    pub failure: bool,
}

#[repr(C, packed)]
#[derive(Default, Copy, Clone)]
pub struct PidUpdate {
    // let's use fixed point format to not waste space
    pub unscaled_p: u16,
    pub unscaled_i: u16,
    pub unscaled_d: u16,
}

impl PidUpdate {
    pub fn get_p(&self) -> f32 {
        return self.unscaled_p as f32 / 100.0;
    }

    pub fn get_i(&self) -> f32 {
        return self.unscaled_i as f32 / 100.0;
    }

    pub fn get_d(&self) -> f32 {
        return self.unscaled_d as f32 / 100.0;
    }
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
