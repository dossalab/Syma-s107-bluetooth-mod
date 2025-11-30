#![no_std]
#![no_main]

use assign_resources::assign_resources;
use ble::events::BluetoothEventsProxy;
use state::SystemState;
use static_cell::StaticCell;

use core::panic::PanicInfo;
use embassy_executor::Spawner;
use embassy_nrf::{
    bind_interrupts,
    interrupt::{self, InterruptExt},
    peripherals, saadc,
    twim::{self, Twim},
    Peri,
};
use embassy_sync::{blocking_mutex::raw::NoopRawMutex, mutex::Mutex, signal::Signal};
use git_version::git_version;
use indications::LedIndicationsSignal;
use nrf_softdevice::{raw, Softdevice};

use defmt::{info, unwrap};

mod ble;
mod control;
mod executor;
mod indications;
mod power;
mod state;
mod xbox;

use defmt_rtt as _;

type SharedI2cBus = Mutex<NoopRawMutex, Twim<'static>>;

bind_interrupts!(struct Irqs {
    TWISPI0 => twim::InterruptHandler<peripherals::TWISPI0>;
    SAADC => saadc::InterruptHandler;
});

assign_resources! {
    led_switch: LedSwitchResources {
        led: P0_00,
        switch: P0_05,
        pwm: PWM1
    },
    i2c: I2cResources {
        // make sure to check interrupt priority below if changing
        i2c: TWISPI0,
        sda: P0_07,
        scl: P0_08,
    },
    power: PowerResources {
        fuelgauge_int: P0_06,
        charging_int: P0_11,
        fault_int: P0_12
    },
    controller: ControllerResources {
        // in current implementation, there's no need to share them, so just
        // keep them here for simplicity
        adc: SAADC,
        pwm: PWM0,

        rotor1: P0_01,
        rotor2: P0_02,
        tail_p: P0_03,
        tail_n: P0_04,
        gyro_power: P0_26,
        gyro_input: P0_28,
        gyro_vref: P0_29,
    },
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

    interrupt::TWISPI0.set_priority(interrupt::Priority::P2);
    interrupt::SAADC.set_priority(interrupt::Priority::P2);

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

fn make_shared_i2c(r: I2cResources) -> &'static SharedI2cBus {
    const BUFFER_LEN: usize = 64;

    static BUS: StaticCell<SharedI2cBus> = StaticCell::new();
    static BUFFER: StaticCell<[u8; BUFFER_LEN]> = StaticCell::new();

    let i2c = Twim::new(
        r.i2c,
        Irqs,
        r.sda,
        r.scl,
        twim::Config::default(),
        BUFFER.init([0; BUFFER_LEN]),
    );

    BUS.init(Mutex::new(i2c))
}

#[embassy_executor::main(executor = "executor::MwuWorkaroundExecutor")]
async fn main(spawner: Spawner) {
    let (r, sd) = hw_init();
    let i2c = make_shared_i2c(r.i2c);

    info!("ble-copter ({}) is running. Hello!", git_version!());

    static POWER_STATS: StaticCell<SystemState> = StaticCell::new();
    let power_stats = POWER_STATS.init(SystemState::new());

    static LED_INDICATIONS: LedIndicationsSignal = Signal::new();
    static BLE_EVENTS: BluetoothEventsProxy = BluetoothEventsProxy::new();

    spawner.spawn(unwrap!(indications::run(&LED_INDICATIONS, r.led_switch)));
    spawner.spawn(unwrap!(ble::run(
        sd,
        power_stats,
        &LED_INDICATIONS,
        &BLE_EVENTS
    )));
    spawner.spawn(unwrap!(control::run(
        &BLE_EVENTS,
        power_stats,
        r.controller,
    )));
    spawner.spawn(unwrap!(power::run(power_stats, r.power, i2c)));
}
