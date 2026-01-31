// Xbox one controller hid defs

use byteorder::{ByteOrder, LittleEndian};
use nrf_softdevice::gatt_client;

use crate::types::{ButtonFlags, JoystickData};

pub const STICKS_RANGE: i32 = 65535;

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

pub fn decode_hid_report(p: &[u8; 16]) -> JoystickData {
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
