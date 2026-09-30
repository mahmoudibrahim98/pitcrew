//! Where the delta stream and activity paging read events: the [`EventSource`] seam, implemented
//! for the hub's store (`StoreSource`, with the default `store` feature) and in memory.

use pitcrew_protocol::events::Event;
use std::fmt;
use std::sync::{Mutex, PoisonError};
use tokio::sync::broadcast;
use tokio::sync::broadcast::error::RecvError;

/// A failure reading the event log.
pub type SourceError = Box<dyn std::error::Error + Send + Sync>;

/// Revisions `from_rev..=to_rev`. Empty when `to_rev < from_rev`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct RevRange {
    /// First revision.
    pub from_rev: u64,
    /// Last revision.
    pub to_rev: u64,
}

impl RevRange {
    /// Whether the range holds no revisions.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.to_rev < self.from_rev
    }
}

/// An event with its revision.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct StoredEvent {
    /// The gap-free revision.
    pub rev: u64,
    /// The event.
    pub event: Event,
}

/// The revision ranges appended to a source from the moment it was subscribed.
#[derive(Debug)]
pub struct Subscription(Receiver);

#[derive(Debug)]
enum Receiver {
    Api(broadcast::Receiver<RevRange>),
    #[cfg(feature = "store")]
    Store(broadcast::Receiver<pitcrew_store::RevRange>),
}

impl Subscription {
    /// Wraps a broadcast receiver of ranges.
    #[must_use]
    pub fn new(receiver: broadcast::Receiver<RevRange>) -> Self {
        Self(Receiver::Api(receiver))
    }

    /// The next range. `Lagged` means ranges were missed and the reader must catch up from the
    /// source; `Closed` means the source is gone.
    ///
    /// # Errors
    /// As `broadcast::Receiver::recv`.
    pub async fn recv(&mut self) -> Result<RevRange, RecvError> {
        match &mut self.0 {
            Receiver::Api(receiver) => receiver.recv().await,
            #[cfg(feature = "store")]
            Receiver::Store(receiver) => receiver.recv().await.map(|r| RevRange {
                from_rev: r.from_rev,
                to_rev: r.to_rev,
            }),
        }
    }
}

/// An append-only event log with gap-free revisions starting at 1.
///
/// Reads are blocking (SQLite); the API calls them from `spawn_blocking`.
pub trait EventSource: Send + Sync + fmt::Debug + 'static {
    /// Identifies the log. It never changes for one log; a new log gets a new id.
    fn log_id(&self) -> String;

    /// The newest revision, or 0 for an empty log.
    ///
    /// # Errors
    /// The log cannot be read.
    fn latest_rev(&self) -> Result<u64, SourceError>;

    /// Up to `limit` events after `rev`, oldest first.
    ///
    /// # Errors
    /// The log cannot be read.
    fn since(&self, rev: u64, limit: usize) -> Result<Vec<StoredEvent>, SourceError>;

    /// The newest `limit` events below `rev` (exclusive), oldest first.
    ///
    /// # Errors
    /// The log cannot be read.
    fn before(&self, rev: u64, limit: usize) -> Result<Vec<StoredEvent>, SourceError>;

    /// The ranges appended from now on. It may report `Lagged`; the reader then catches up with
    /// [`EventSource::since`].
    fn subscribe(&self) -> Subscription;
}

#[cfg(feature = "store")]
pub use store_source::StoreSource;

#[cfg(feature = "store")]
mod store_source {
    use super::{EventSource, SourceError, StoredEvent, Subscription};
    use pitcrew_store::{EventFilter, Store};
    use std::sync::Arc;

    /// The hub's store as an event source.
    ///
    /// The log id is given by the caller until `Store::log_id()` exists (brief C-projections).
    #[derive(Debug, Clone)]
    pub struct StoreSource {
        store: Arc<Store>,
        log_id: String,
    }

    impl StoreSource {
        /// Reads `store`, whose log is identified by `log_id`.
        #[must_use]
        pub fn new(store: Arc<Store>, log_id: impl Into<String>) -> Self {
            Self {
                store,
                log_id: log_id.into(),
            }
        }
    }

    fn convert(events: Vec<pitcrew_store::StoredEvent>) -> Vec<StoredEvent> {
        events
            .into_iter()
            .map(|e| StoredEvent {
                rev: e.rev,
                event: e.event,
            })
            .collect()
    }

    impl EventSource for StoreSource {
        fn log_id(&self) -> String {
            self.log_id.clone()
        }

        fn latest_rev(&self) -> Result<u64, SourceError> {
            Ok(self.store.latest_rev()?)
        }

        fn since(&self, rev: u64, limit: usize) -> Result<Vec<StoredEvent>, SourceError> {
            Ok(convert(self.store.since(rev, limit)?))
        }

        fn before(&self, rev: u64, limit: usize) -> Result<Vec<StoredEvent>, SourceError> {
            Ok(convert(self.store.before(
                rev,
                limit,
                &EventFilter::default(),
            )?))
        }

        fn subscribe(&self) -> Subscription {
            Subscription(super::Receiver::Store(self.store.subscribe()))
        }
    }
}

/// An in-memory event log, for tests and development daemons.
#[derive(Debug)]
pub struct MemorySource {
    log_id: String,
    events: Mutex<Vec<Event>>,
    revs: broadcast::Sender<RevRange>,
}

impl MemorySource {
    /// An empty log named `log_id`, whose subscribers lag after `capacity` unread ranges.
    #[must_use]
    pub fn new(log_id: impl Into<String>, capacity: usize) -> Self {
        let (revs, _) = broadcast::channel(capacity.max(1));
        Self {
            log_id: log_id.into(),
            events: Mutex::new(Vec::new()),
            revs,
        }
    }

    /// Appends events and tells subscribers, like `Store::append`.
    pub fn append(&self, events: Vec<Event>) -> RevRange {
        let range = self.append_quietly(events);
        if !range.is_empty() {
            let _ = self.revs.send(range);
        }
        range
    }

    /// Appends events without telling subscribers. For tests that announce ranges themselves.
    #[doc(hidden)]
    pub fn append_quietly(&self, events: Vec<Event>) -> RevRange {
        let mut log = self.events.lock().unwrap_or_else(PoisonError::into_inner);
        let from_rev = (log.len() as u64).saturating_add(1);
        log.extend(events);
        RevRange {
            from_rev,
            to_rev: log.len() as u64,
        }
    }

    fn slice(&self, from: usize, to: usize) -> Vec<StoredEvent> {
        let log = self.events.lock().unwrap_or_else(PoisonError::into_inner);
        let to = to.min(log.len());
        let from = from.min(to);
        log[from..to]
            .iter()
            .zip((from as u64).saturating_add(1)..)
            .map(|(event, rev)| StoredEvent {
                rev,
                event: event.clone(),
            })
            .collect()
    }
}

impl EventSource for MemorySource {
    fn log_id(&self) -> String {
        self.log_id.clone()
    }

    fn latest_rev(&self) -> Result<u64, SourceError> {
        let log = self.events.lock().unwrap_or_else(PoisonError::into_inner);
        Ok(log.len() as u64)
    }

    fn since(&self, rev: u64, limit: usize) -> Result<Vec<StoredEvent>, SourceError> {
        // Revision r is at index r - 1, so the events after `rev` start at index `rev`.
        let from = usize::try_from(rev).unwrap_or(usize::MAX);
        Ok(self.slice(from, from.saturating_add(limit)))
    }

    fn before(&self, rev: u64, limit: usize) -> Result<Vec<StoredEvent>, SourceError> {
        // Events below `rev` end at index `rev - 1`, or at the end of the log.
        let len = self
            .events
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .len();
        let to = usize::try_from(rev.saturating_sub(1))
            .unwrap_or(usize::MAX)
            .min(len);
        Ok(self.slice(to.saturating_sub(limit), to))
    }

    fn subscribe(&self) -> Subscription {
        Subscription::new(self.revs.subscribe())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use pitcrew_protocol::events::EventBody;
    use pitcrew_protocol::model::Liveness;
    use pitcrew_protocol::{MachineId, MemberId, WorkspaceId};

    fn events(n: usize) -> Vec<Event> {
        (0..n)
            .map(|_| {
                Event::now(
                    WorkspaceId::new(),
                    MemberId::new(),
                    EventBody::MachineLiveness {
                        machine: MachineId::new(),
                        liveness: Liveness::Live,
                    },
                )
            })
            .collect()
    }

    fn revs(events: &[StoredEvent]) -> Vec<u64> {
        events.iter().map(|e| e.rev).collect()
    }

    #[test]
    fn memory_source_reads_both_ways() {
        let source = MemorySource::new("log", 4);
        source.append(events(10));
        assert_eq!(revs(&source.since(0, 3).unwrap()), vec![1, 2, 3]);
        assert_eq!(revs(&source.since(8, 5).unwrap()), vec![9, 10]);
        assert!(source.since(10, 5).unwrap().is_empty());
        assert_eq!(revs(&source.before(11, 3).unwrap()), vec![8, 9, 10]);
        assert_eq!(revs(&source.before(4, 10).unwrap()), vec![1, 2, 3]);
        assert!(source.before(1, 10).unwrap().is_empty());
        assert_eq!(revs(&source.before(u64::MAX, 2).unwrap()), vec![9, 10]);
    }
}
