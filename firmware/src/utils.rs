use core::future::Future;

use embassy_futures::select::select;
use scopeguard::guard;

use crate::state::{StateReceiver, StateSender};

/// Runs `fun` and publishes its lifecycle to `sender`: sends `true` on start,
/// `false` on completion (or cancellation via drop).
pub async fn run_reported<F: Future>(sender: StateSender<'_, bool>, fun: F) -> F::Output {
    sender.send(true);
    let _guard = guard(sender, |s| s.send(false));
    fun.await
}

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
