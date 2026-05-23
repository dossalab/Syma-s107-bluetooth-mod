use embassy_time::Instant;

include!(concat!(env!("OUT_DIR"), "/gatt_generated.rs"));

impl Default for Configuration {
    fn default() -> Self {
        Self {
            p: 0.5,
            i: 0.2,
            d: 0.2,
            yaw_expo: 0.6,
        }
    }
}

impl Default for RaTableUpdate {
    fn default() -> Self {
        Self {
            timestamp: SecInstant { sec: 0 },
        }
    }
}

impl Default for QmaxUpdate {
    fn default() -> Self {
        Self {
            timestamp: SecInstant { sec: 0 },
            value: 0,
        }
    }
}

impl Default for OcvMeasurement {
    fn default() -> Self {
        Self {
            timestamp: SecInstant { sec: 0 },
        }
    }
}

impl From<Instant> for SecInstant {
    fn from(value: Instant) -> Self {
        Self {
            sec: value.as_secs() as u32,
        }
    }
}
