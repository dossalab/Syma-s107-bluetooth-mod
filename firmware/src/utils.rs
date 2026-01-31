use embassy_futures::select::select;

use crate::state::StateReceiver;

pub async fn run_with_receiver<'a, F>(mut receiver: StateReceiver<'a, bool>, mut fun: F)
where
    F: AsyncFnMut(),
{
    loop {
        if let Some(true) = receiver.try_get() {
            let mut wait_cancellation = async || loop {
                let cond = receiver.changed().await;
                if !cond {
                    break;
                }
            };

            select(fun(), wait_cancellation()).await;
            continue;
        }

        _ = receiver.changed().await;
    }
}
