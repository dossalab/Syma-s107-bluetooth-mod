use embassy_time::Instant;

include!(concat!(env!("OUT_DIR"), "/gatt_generated.rs"));

impl From<Instant> for SecInstant {
    fn from(value: Instant) -> Self {
        Self {
            sec: value.as_secs() as u32,
        }
    }
}
