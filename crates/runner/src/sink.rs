//! Where events go, and the thread that hands them over and then saves the cursors.

use crate::store::{Commit, Store};
use pitcrew_protocol::events::Event;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::mpsc::Receiver;
use std::sync::{Arc, Mutex};
use std::time::Duration;

/// Receives the runner's events, in order. The hub link implements this in a later brief.
///
/// `accept` may block (that is the backpressure), but should return eventually. Returning `Ok`
/// means the events are durably taken: the runner then saves its cursor, and will not send them
/// again unless it crashes first. After a crash, the unsaved tail is sent again with the **same
/// event ids**, so a receiver dedupes by id (or by session and transcript offset).
pub trait EventSink: Send + Sync {
    /// Takes a batch of events. On `Err` the same batch is offered again later.
    fn accept(&self, events: &[Event]) -> Result<(), SinkError>;
}

/// A sink could not take a batch right now.
#[derive(Debug, thiserror::Error)]
#[error("event sink: {0}")]
pub struct SinkError(pub String);

/// Events to deliver, and what to save once they are delivered.
#[derive(Debug)]
pub(crate) struct Batch {
    pub events: Vec<Event>,
    pub commit: Commit,
    /// Batches of the transcript's whole row in memory still to save (the watcher's `Loaded`):
    /// lowered once this one's commit is saved. A commit that fails leaves it raised, so that
    /// row, ahead of the index, stays in memory.
    pub unsaved: Option<Arc<AtomicUsize>>,
}

/// Delivers batches until the watcher hangs up, or until `stop` while the sink is refusing.
pub(crate) fn dispatch(
    rx: &Receiver<Batch>,
    sink: &dyn EventSink,
    store: &Mutex<Store>,
    stop: &AtomicBool,
    retry_max: Duration,
) {
    for batch in rx {
        if !batch.events.is_empty() {
            let mut wait = Duration::from_millis(50);
            loop {
                match sink.accept(&batch.events) {
                    Ok(()) => break,
                    Err(e) => {
                        if stop.load(Ordering::Relaxed) {
                            // Not saved: these events are sent again after the restart.
                            return;
                        }
                        tracing::warn!(error = %e, retry_in = ?wait, "event sink refused a batch");
                        std::thread::sleep(wait);
                        wait = (wait * 2).min(retry_max);
                    }
                }
            }
        }
        let saved = store
            .lock()
            .map_err(|_| "runner store lock poisoned".to_owned())
            .and_then(|s| s.commit(&batch.commit).map_err(|e| e.to_string()));
        match saved {
            Ok(()) => {
                if let Some(unsaved) = &batch.unsaved {
                    unsaved.fetch_sub(1, Ordering::AcqRel);
                }
            }
            // The events went out but the cursor did not move on disk: they repeat after a
            // restart, which receivers already handle.
            Err(e) => tracing::error!(error = %e, "could not save a transcript cursor"),
        }
    }
}

/// Shares a sink between the runner and the caller.
impl<T: EventSink + ?Sized> EventSink for Arc<T> {
    fn accept(&self, events: &[Event]) -> Result<(), SinkError> {
        (**self).accept(events)
    }
}
