//! Where the delta stream reads events: the [`EventSource`] seam, implemented for the hub's
//! [`Store`] and, for tests and development, in memory.

use pitcrew_protocol::events::Event;
use pitcrew_store::Store;
pub use pitcrew_store::{RevRange, StoredEvent};
use std::fmt;
use std::sync::{Arc, Mutex, PoisonError};
use tokio::sync::broadcast;

/// A failure reading the event log.
pub type SourceError = Box<dyn std::error::Error + Send + Sync>;

/// An append-only event log with gap-free revisions starting at 1.
///
/// Reads are blocking (SQLite); the stream calls them from `spawn_blocking`.
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

    /// A receiver of the revision ranges appended from now on. It may report `Lagged`; the
    /// reader then catches up with [`EventSource::since`].
    fn subscribe(&self) -> broadcast::Receiver<RevRange>;
}

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

impl EventSource for StoreSource {
    fn log_id(&self) -> String {
        self.log_id.clone()
    }

    fn latest_rev(&self) -> Result<u64, SourceError> {
        Ok(self.store.latest_rev()?)
    }

    fn since(&self, rev: u64, limit: usize) -> Result<Vec<StoredEvent>, SourceError> {
        Ok(self.store.since(rev, limit)?)
    }

    fn subscribe(&self) -> broadcast::Receiver<RevRange> {
        self.store.subscribe()
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
        let mut log = self.events.lock().unwrap_or_else(PoisonError::into_inner);
        let from_rev = log.len() as u64 + 1;
        log.extend(events);
        let range = RevRange {
            from_rev,
            to_rev: log.len() as u64,
        };
        drop(log);
        if !range.is_empty() {
            let _ = self.revs.send(range);
        }
        range
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
        let log = self.events.lock().unwrap_or_else(PoisonError::into_inner);
        let start = usize::try_from(rev).unwrap_or(usize::MAX).min(log.len());
        Ok(log[start..]
            .iter()
            .take(limit)
            .zip(rev + 1..)
            .map(|(event, rev)| StoredEvent {
                rev,
                event: event.clone(),
            })
            .collect())
    }

    fn subscribe(&self) -> broadcast::Receiver<RevRange> {
        self.revs.subscribe()
    }
}
