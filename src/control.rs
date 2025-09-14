use crate::{xbox::JoystickDataSignal, MotorResources};

#[embassy_executor::task]
pub async fn run(output: &'static JoystickDataSignal, motor: MotorResources) {}
