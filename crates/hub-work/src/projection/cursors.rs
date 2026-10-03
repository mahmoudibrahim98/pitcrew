//! Forward-only read metadata, rebuilt from the log.

use super::{clear, exec};
use crate::codec::{IdText, sql_rev};
use pitcrew_protocol::events::EventBody;
use pitcrew_store::sql::{Transaction, params};
use pitcrew_store::{BoxError, Projection, StoredEvent};

/// Per-person, per-scope read cursors.
#[derive(Debug, Clone, Copy, Default)]
pub struct Cursors;

impl Cursors {
    /// The projection's name.
    pub const NAME: &'static str = "work.cursors";
}

impl Projection for Cursors {
    fn name(&self) -> &str {
        Self::NAME
    }
    fn version(&self) -> u32 {
        1
    }
    fn reset(&self, tx: &Transaction<'_>) -> Result<(), BoxError> {
        clear(tx, &["work_read_cursors"])
    }
    fn apply(&self, tx: &Transaction<'_>, stored: &StoredEvent) -> Result<(), BoxError> {
        if let EventBody::CursorMoved { scope, rev } = &stored.event.body {
            exec(
                tx,
                "INSERT INTO work_read_cursors (member, scope, rev) VALUES (?1, ?2, ?3)
                 ON CONFLICT (member, scope) DO UPDATE SET rev = MAX(rev, excluded.rev)",
                params![stored.event.author.text(), scope, sql_rev(*rev)],
            )?;
        }
        Ok(())
    }
}
