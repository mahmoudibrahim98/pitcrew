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
    group_events: usize,
) {
    let mut pending = None;
    loop {
        let Some(first) = pending.take().or_else(|| rx.recv().ok()) else {
            return;
        };
        let complete = matches!(&first.commit, Commit::Full(row) if row.caught_up);
        let mut count = first.events.len();
        let mut batches = vec![first];
        if group_events > 0 && complete {
            while batches.len() < 4 && count < group_events {
                // One bounded wait lets the watcher finish the other files from this wakeup.
                let next = match rx.recv_timeout(Duration::from_millis(1)) {
                    Ok(next) => next,
                    Err(_) => break,
                };
                let other_complete = match &next.commit {
                    Commit::Full(row) if row.caught_up => batches.iter().all(|batch| {
                        !matches!(&batch.commit, Commit::Full(previous) if previous.session == row.session)
                    }),
                    _ => false,
                };
                if !other_complete || count.saturating_add(next.events.len()) > group_events {
                    pending = Some(next);
                    break;
                }
                count += next.events.len();
                batches.push(next);
            }
        }
        let mut events = std::mem::take(&mut batches[0].events);
        for batch in &mut batches[1..] {
            events.append(&mut batch.events);
        }
        if !events.is_empty() {
            let mut wait = Duration::from_millis(50);
            loop {
                match sink.accept(&events) {
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
            .and_then(|s| {
                if batches.len() == 1 {
                    s.commit(&batches[0].commit)
                } else {
                    s.commit_many(batches.iter().map(|b| &b.commit))
                }
                .map_err(|e| e.to_string())
            });
        match saved {
            Ok(()) => {
                for batch in batches {
                    if let Some(unsaved) = &batch.unsaved {
                        unsaved.fetch_sub(1, Ordering::AcqRel);
                    }
                }
            }
            // Accepted events whose transaction failed repeat with their existing ids.
            Err(e) => tracing::error!(error = %e, "could not save transcript cursors"),
        }
    }
}

/// Shares a sink between the runner and the caller.
impl<T: EventSink + ?Sized> EventSink for Arc<T> {
    fn accept(&self, events: &[Event]) -> Result<(), SinkError> {
        (**self).accept(events)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::derive::Facts;
    use crate::store::Row;
    use pitcrew_interfaces::source::Cursor;
    use pitcrew_protocol::events::EventBody;
    use pitcrew_protocol::ids::{EventId, MemberId, SessionId, WorkspaceId};
    use pitcrew_protocol::model::Engine;
    use std::collections::HashSet;

    #[derive(Default)]
    struct RefuseOnce {
        refused: AtomicBool,
        offered: Mutex<Vec<Vec<EventId>>>,
    }

    impl EventSink for RefuseOnce {
        fn accept(&self, events: &[Event]) -> Result<(), SinkError> {
            self.offered
                .lock()
                .unwrap()
                .push(events.iter().map(|e| e.id).collect());
            if !self.refused.swap(true, Ordering::Relaxed) {
                return Err(SinkError("retry this group".into()));
            }
            Ok(())
        }
    }

    #[test]
    fn completed_sessions_group_within_the_limit_and_retry_without_losing_cursors() {
        let dir = tempfile::tempdir().unwrap();
        let store = Mutex::new(Store::open(dir.path()).unwrap());
        let (tx, rx) = std::sync::mpsc::channel();
        let mut rows = Vec::new();
        let mut counters = Vec::new();
        for n in 0..5 {
            let mut row = Row {
                session: SessionId::new(),
                engine: Engine::Claude,
                path: dir.path().join(format!("{n}.jsonl")),
                inner_id: None,
                cursor: Arc::default(),
                size: 0,
                mtime: 0,
                identity: None,
                caught_up: true,
                generation: 0,
                discovered: true,
                accepted: HashSet::new(),
                meta: None,
                facts: Facts::default(),
            };
            store.lock().unwrap().insert(&row).unwrap();
            row.cursor = Arc::new(Cursor {
                offset: n + 1,
                state: None,
            });
            let unsaved = Arc::new(AtomicUsize::new(1));
            tx.send(Batch {
                events: vec![Event::now(
                    WorkspaceId::new(),
                    MemberId::new(),
                    EventBody::SessionEnded {
                        session: row.session,
                    },
                )],
                commit: Commit::Full(Box::new(row.clone())),
                unsaved: Some(unsaved.clone()),
            })
            .unwrap();
            counters.push(unsaved);
            rows.push(row);
        }
        drop(tx);
        let sink = RefuseOnce::default();
        dispatch(
            &rx,
            &sink,
            &store,
            &AtomicBool::new(false),
            Duration::from_millis(50),
            4,
        );
        let offered = sink.offered.lock().unwrap();
        assert_eq!(offered.iter().map(Vec::len).collect::<Vec<_>>(), [4, 4, 1]);
        assert_eq!(offered[0], offered[1], "retry the identical ordered group");
        for (row, pending) in rows.iter().zip(counters) {
            assert_eq!(
                store
                    .lock()
                    .unwrap()
                    .load(row.session)
                    .unwrap()
                    .unwrap()
                    .cursor,
                row.cursor
            );
            assert_eq!(
                pending.load(Ordering::Acquire),
                0,
                "only saved rows released"
            );
        }
    }
}
