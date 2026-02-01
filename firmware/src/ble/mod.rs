use central::{central_loop, Bonder};
use defmt::unwrap;
use embassy_futures::join::join3;
use nrf_softdevice::Softdevice;
use peripheral::{peripheral_loop, GattServer};
use static_cell::StaticCell;

use crate::state::SystemState;

mod central;
mod errors;
mod peripheral;

#[embassy_executor::task]
pub async fn run(sd: &'static mut Softdevice, state: &'static SystemState) {
    static BONDER: StaticCell<Bonder> = StaticCell::new();
    let bonder = BONDER.init(Bonder::default());
    let server = unwrap!(GattServer::new(sd));

    join3(
        central_loop(sd, state, bonder),
        peripheral_loop(sd, state, &server),
        sd.run(),
    )
    .await;
}
