#![no_std]
#![no_main]

use assign_resources::assign_resources;
use xbox::JoystickDataSignal;

use core::panic::PanicInfo;
use embassy_executor::Spawner;
use embassy_nrf::{interrupt, peripherals, Peri};
use embassy_sync::signal::Signal;
use git_version::git_version;
use indications::LedIndicationsSignal;
use nrf_softdevice::Softdevice;

use defmt::{info, unwrap};

mod ble;
mod control;
mod indications;
mod xbox;

use defmt_rtt as _;

assign_resources! {
    motor: MotorResources {
        rotor1: P0_01,
        rotor2: P0_02,
        tail_p: P0_03,
        tail_n: P0_04,
    },
    led_switch: LedSwitchResources {
        led: P0_00,
        switch: P0_05,
        pwm: PWM0
    },
    fuelgauge: FuelgaugeResources {
        int: P0_06,
        sda: P0_07,
        scl: P0_08,
    },
    charger: ChargerResources {
        charge: P0_11,
        error: P0_12
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

fn hw_init() -> (AssignedResources, &'static Softdevice) {
    let mut config = embassy_nrf::config::Config::default();

    /*
     * Softdevice implicitly utilizes the highest-level interrupt priority
     * We have to move all other interrupts to lower priority, unless
     * random issues and asserts from the Softdevice may (and will) occur
     */
    config.gpiote_interrupt_priority = interrupt::Priority::P2;
    config.time_interrupt_priority = interrupt::Priority::P2;

    let p = embassy_nrf::init(config);
    let sd = Softdevice::enable(&nrf_softdevice::Config::default());

    (split_resources!(p), sd)
}

#[embassy_executor::main]
async fn main(spawner: Spawner) {
    let (r, sd) = hw_init();

    info!("ble-copter ({}) is running. Hello!", git_version!());

    static LED_INDICATIONS_SIGNAL: LedIndicationsSignal = Signal::new();
    static JOYSTICK_SIGNAL: JoystickDataSignal = Signal::new();

    unwrap!(spawner.spawn(indications::run(&LED_INDICATIONS_SIGNAL, r.led_switch)));
    unwrap!(spawner.spawn(ble::run(sd, &LED_INDICATIONS_SIGNAL, &JOYSTICK_SIGNAL)));
    unwrap!(spawner.spawn(control::run(&JOYSTICK_SIGNAL, r.motor)));

    sd.run().await
}
