use central::{central_loop, Bonder};
use defmt::unwrap;
use embassy_futures::join::join3;
use events::BluetoothEventsProxy;
use nrf_softdevice::Softdevice;
use peripheral::{peripheral_loop, GattServer};
use static_cell::StaticCell;

use crate::{indications::LedIndicationsSignal, state::SystemState};

mod central;
mod errors;
mod peripheral;

pub mod events;

#[embassy_executor::task]
pub async fn run(
    sd: &'static mut Softdevice,
    ps: &'static SystemState,
    indications: &'static LedIndicationsSignal,
    events: &'static BluetoothEventsProxy,
) {
    static BONDER: StaticCell<Bonder> = StaticCell::new();
    let bonder = BONDER.init(Bonder::default());
    let server = unwrap!(GattServer::new(sd));

    join3(
        central_loop(sd, indications, events, bonder),
        peripheral_loop(sd, ps, server),
        sd.run(),
    )
    .await;
}
