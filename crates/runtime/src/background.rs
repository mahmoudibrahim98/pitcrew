//! Blocking work on its own thread, as a future any executor can await without blocking.

use std::future::Future;
use std::pin::Pin;
use std::sync::{Arc, Mutex};
use std::task::{Context, Poll, Waker};

use crate::lock;

/// The result of work running on its own thread. Polling it never blocks.
#[derive(Debug)]
pub struct Background<T> {
    slot: Arc<Mutex<Slot<T>>>,
}

#[derive(Debug)]
struct Slot<T> {
    result: Option<T>,
    waker: Option<Waker>,
}

/// Runs `work` on a new thread named `name`. If no thread can be started, the future resolves
/// to `failed(reason)` at once.
pub(crate) fn spawn<T: Send + 'static>(
    name: &str,
    work: impl FnOnce() -> T + Send + 'static,
    failed: impl FnOnce(String) -> T,
) -> Background<T> {
    let slot = Arc::new(Mutex::new(Slot {
        result: None,
        waker: None,
    }));
    let filled = Arc::clone(&slot);
    let spawned = std::thread::Builder::new()
        .name(name.to_owned())
        .spawn(move || {
            let result = work();
            let waker = {
                let mut slot = lock(&filled);
                slot.result = Some(result);
                slot.waker.take()
            };
            if let Some(waker) = waker {
                waker.wake();
            }
        });
    if let Err(e) = spawned {
        lock(&slot).result = Some(failed(format!("cannot start a thread: {e}")));
    }
    Background { slot }
}

impl<T> Future for Background<T> {
    type Output = T;

    fn poll(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<T> {
        let mut slot = lock(&self.slot);
        match slot.result.take() {
            Some(result) => Poll::Ready(result),
            None => {
                slot.waker = Some(cx.waker().clone());
                Poll::Pending
            }
        }
    }
}
