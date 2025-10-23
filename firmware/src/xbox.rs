// Xbox one controller hid defs

use byteorder::{ByteOrder, LittleEndian};
use defmt::bitflags;
use nrf_softdevice::gatt_client;

pub const STICKS_RANGE: i32 = 65535;

bitflags! {
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

#[derive(defmt::Format)]
pub struct JoystickData {
    pub j1: (i32, i32),
    pub j2: (i32, i32),
    pub t1: u16,
    pub t2: u16,
    pub buttons: ButtonFlags,
}

impl JoystickData {
    pub fn from_packet(p: &[u8; 16]) -> Self {
        let button_mask = LittleEndian::read_u24(&p[13..16]);

        let x1 = LittleEndian::read_u16(&p[0..2]);
        let y1 = LittleEndian::read_u16(&p[2..4]);
        let x2 = LittleEndian::read_u16(&p[4..6]);
        let y2 = LittleEndian::read_u16(&p[6..8]);

        let t1 = LittleEndian::read_u16(&p[8..10]);
        let t2 = LittleEndian::read_u16(&p[10..12]);

        let map_stick = |x| (x as i32) - STICKS_RANGE / 2;

        JoystickData {
            j1: (map_stick(x1), -map_stick(y1)),
            j2: (map_stick(x2), -map_stick(y2)),
            t1,
            t2,
            buttons: ButtonFlags::from_bits_truncate(button_mask),
        }
    }
}
#[gatt_client(uuid = "1812")]
pub struct XboxHidServiceClient {
    #[characteristic(uuid = "2a4b", read)]
    pub hid_report_map: [u8; 64],

    #[characteristic(uuid = "2a4d", read, notify)]
    pub hid_report: [u8; 16],
}

// Checks whether advetrisement packet is coming from XBox controller
// This is a pretty crude check overall.
pub fn is_xbox_controller(packet: &[u8]) -> bool {
    const TYPE_MANUFACTURER_SPECIFIC_DATA: u8 = 0xFF;
    const TYPE_PARTIAL_16BIT_UUIDS: u8 = 0x02;
    const TYPE_COMPLETE_16BIT_UUIDS: u8 = 0x03;

    let mut i = 0;

    let mut next_entry = || {
        let mut remaining = packet.len() - i;

        // we need at least len + type
        if remaining < 2 {
            i += remaining;
            None
        } else {
            let data_len = packet[i] as usize;
            i += 1;
            remaining -= 1;

            if data_len == 0 || data_len > remaining {
                i += remaining;
                return None;
            }

            let data = &packet[i..i + data_len];
            i += data_len;

            Some((data[0], &data[1..]))
        }
    };

    let mut is_microsoft = false;
    let mut is_hid = false;

    while let Some((t, data)) = next_entry() {
        match t {
            TYPE_MANUFACTURER_SPECIFIC_DATA => {
                if data.len() >= 2 && data[0..2] == [0x06, 0x00] {
                    is_microsoft = true;
                }
            }

            TYPE_PARTIAL_16BIT_UUIDS | TYPE_COMPLETE_16BIT_UUIDS => {
                for uuid in data.chunks(2) {
                    if uuid == [0x12, 0x18] {
                        is_hid = true;
                    }
                }
            }
            _ => {}
        }
    }

    is_microsoft && is_hid
}
