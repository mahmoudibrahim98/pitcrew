//! [`StoreSink`]: the runner writing straight into the hub's store, when both run in one process
//! (the solo case in ADR-0009).

use crate::sink::{EventSink, SinkError};
use pitcrew_protocol::events::Event;
use pitcrew_protocol::ids::MemberId;
use pitcrew_store::Store;
use std::error::Error as _;
use std::fmt;
use std::sync::Arc;

/// An [`EventSink`] that appends accepted batches to the hub's [`Store`].
///
/// - A batch sent again after a crash (or after an error whose outcome is unknown) is stored
///   once: `append_new` skips the events whose ids are already in the log, and the runner
///   repeats ids.
/// - Events are stamped as the hub stamps a caller's: those of sessions without an agent (the
///   ones with no `on_behalf_of`) are authored by the configured owner.
pub struct StoreSink {
    store: Arc<Store>,
    owner: MemberId,
}

impl StoreSink {
    /// Appends to `store`, authoring agentless sessions' events as `owner`.
    #[must_use]
    pub fn new(store: Arc<Store>, owner: MemberId) -> Self {
        Self { store, owner }
    }
}

impl fmt::Debug for StoreSink {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("StoreSink")
            .field("log", &self.store.log_id())
            .field("owner", &self.owner)
            .finish()
    }
}

impl EventSink for StoreSink {
    fn accept(&self, events: &[Event]) -> Result<(), SinkError> {
        let stamped: Vec<Event> = events
            .iter()
            .map(|e| {
                let mut e = e.clone();
                if e.on_behalf_of.is_none() {
                    e.author = self.owner;
                }
                e
            })
            .collect();
        match self.store.append_new(&stamped) {
            Ok((_, skipped)) => {
                if !skipped.is_empty() {
                    tracing::debug!(
                        skipped = skipped.len(),
                        "events already in the log were not stored again"
                    );
                }
                Ok(())
            }
            Err(e) => {
                // Store errors keep their cause in `source()`.
                let mut message = e.to_string();
                let mut cause = e.source();
                while let Some(c) = cause {
                    message.push_str(": ");
                    message.push_str(&c.to_string());
                    cause = c.source();
                }
                Err(SinkError(message))
            }
        }
    }
}
