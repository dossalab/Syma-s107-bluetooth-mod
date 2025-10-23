use core::future::Future;

use embassy_sync::{blocking_mutex::raw::CriticalSectionRawMutex, signal::Signal};

use crate::xbox::JoystickData;

pub struct BluetoothEventsProxy {
    connect: Signal<CriticalSectionRawMutex, ()>,
    disconnect: Signal<CriticalSectionRawMutex, ()>,
    data: Signal<CriticalSectionRawMutex, JoystickData>,
}

impl BluetoothEventsProxy {
    pub async fn wait_connect(&self) {
        self.connect.wait().await
    }

    pub async fn wait_disconnect(&self) {
        self.disconnect.wait().await
    }

    pub async fn wait_joystick_data(&self) -> JoystickData {
        self.data.wait().await
    }

    pub async fn notify_connection<F, R, Fut>(&self, f: F) -> R
    where
        F: FnOnce() -> Fut,
        Fut: Future<Output = R>,
    {
        self.connect.signal(());
        let r = f().await;
        self.disconnect.signal(());
        r
    }

    pub fn notify_data(&self, data: JoystickData) {
        self.data.signal(data);
    }

    pub const fn new() -> Self {
        Self {
            connect: Signal::new(),
            disconnect: Signal::new(),
            data: Signal::new(),
        }
    }
}
