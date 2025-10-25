#![no_std]
#![no_main]

use assign_resources::assign_resources;
use ble_events::BluetoothEventsProxy;

use core::panic::PanicInfo;
use embassy_executor::Spawner;
use embassy_nrf::{bind_interrupts, interrupt, peripherals, twim, Peri};
use embassy_sync::signal::Signal;
use git_version::git_version;
use indications::LedIndicationsSignal;
use nrf_softdevice::{raw, Softdevice};

use defmt::{info, unwrap};

mod ble;
mod ble_events;
mod control;
mod executor;
mod indications;
mod power;
mod xbox;

use defmt_rtt as _;

bind_interrupts!(struct Irqs {
    TWISPI0 => twim::InterruptHandler<peripherals::TWISPI0>;
});

assign_resources! {
    motor: MotorResources {
        pwm: PWM0,
        rotor1: P0_01,
        rotor2: P0_02,
        tail_p: P0_03,
        tail_n: P0_04,
    },
    led_switch: LedSwitchResources {
        led: P0_00,
        switch: P0_05,
        pwm: PWM1
    },
    fuelgauge: FuelgaugeResources {
        i2c: TWISPI0,
        int: P0_06,
        sda: P0_07,
        scl: P0_08,
    },
    charger: ChargerResources {
        charging: P0_11,
        fault: P0_12
    },
    gyro: GyroResources {
        power: P0_26,
        input: P0_28,
        vref: P0_29
    }
}

// It's safer to reboot rather than hang
#[panic_handler]
fn panic(_info: &PanicInfo) -> ! {
    cortex_m::peripheral::SCB::sys_reset();
}

fn hw_init() -> (AssignedResources, &'static mut Softdevice) {
    let mut config = embassy_nrf::config::Config::default();

    /*
     * Softdevice implicitly utilizes the highest-level interrupt priority
     * We have to move all other interrupts to lower priority, unless
     * random issues and asserts from the Softdevice may (and will) occur
     */
    config.gpiote_interrupt_priority = interrupt::Priority::P2;
    config.time_interrupt_priority = interrupt::Priority::P2;

    let sd_config = nrf_softdevice::Config {
        conn_gap: Some(raw::ble_gap_conn_cfg_t {
            conn_count: 2,
            event_length: 24,
        }),
        ..nrf_softdevice::Config::default()
    };

    let p = embassy_nrf::init(config);
    let sd = Softdevice::enable(&sd_config);

    (split_resources!(p), sd)
}

#[embassy_executor::main(executor = "executor::MwuWorkaroundExecutor")]
async fn main(spawner: Spawner) {
    let (r, sd) = hw_init();

    info!("ble-copter ({}) is running. Hello!", git_version!());

    static LED_INDICATIONS: LedIndicationsSignal = Signal::new();
    static BLE_EVENTS: BluetoothEventsProxy = BluetoothEventsProxy::new();

    spawner.spawn(unwrap!(indications::run(&LED_INDICATIONS, r.led_switch)));
    spawner.spawn(unwrap!(ble::run(sd, &LED_INDICATIONS, &BLE_EVENTS)));
    spawner.spawn(unwrap!(control::run(&BLE_EVENTS, r.motor)));

    spawner.spawn(unwrap!(power::run(
        &LED_INDICATIONS,
        r.fuelgauge,
        r.charger
    )));
}
