//! Projections: tables derived from the event log and kept in step with it.
//!
//! A domain crate implements [`Projection`] over its own tables (created by its own migrations)
//! and passes it to [`Store::open_with`](crate::Store::open_with). The store then:
//! - applies every appended event to it **inside the append's transaction**, so the log and the
//!   projection never disagree; a failing `apply` rolls the append back;
//! - records how far each projection got in `projection_state (name, version, rev)`;
//! - on open, rebuilds a projection whose `version` changed (`reset`, then replay the log) and
//!   catches up one that is behind the log.
//!
//! Projections write SQL through [`crate::sql`], the store's own `rusqlite`, so every crate uses
//! the workspace's version and the same transaction type.

use crate::error::{Error, Result};
use crate::sql::{OptionalExtension, Transaction};
use crate::store::{StoredEvent, read_events_after};

/// Any error a projection returns. It becomes [`Error::Projection`].
pub type BoxError = Box<dyn std::error::Error + Send + Sync>;

/// How many events a rebuild or catch-up reads from the log at a time.
const REPLAY_BATCH: usize = 1_000;

/// Tables derived from the event log. See the [module docs](self).
///
/// `reset` followed by `apply` for every event in the log, in revision order, must give the same
/// tables as applying the events one append at a time. Both run inside a write transaction: do
/// not commit, and do not touch other projections' tables or the `events` table.
pub trait Projection: Send + Sync {
    /// A stable, unique name, e.g. `work.tasks`. It keys the projection's checkpoint.
    fn name(&self) -> &str;

    /// The projection's version. Bump it when `apply` changes meaning; the next open rebuilds
    /// the projection from the log.
    fn version(&self) -> u32;

    /// Clears the projection's tables, before a rebuild.
    ///
    /// # Errors
    ///
    /// Any error aborts the rebuild, and the open or [`Store::rebuild`](crate::Store::rebuild)
    /// that started it.
    fn reset(&self, tx: &Transaction<'_>) -> std::result::Result<(), BoxError>;

    /// Applies one event. Events come in revision order, each exactly once. Ignore event types
    /// the projection does not care about.
    ///
    /// # Errors
    ///
    /// Any error rolls back the append (or rebuild) it is part of. It means a bug, so it is never
    /// skipped.
    fn apply(&self, tx: &Transaction<'_>, event: &StoredEvent)
    -> std::result::Result<(), BoxError>;
}

/// A projection's checkpoint: its version and the last revision applied to it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct Checkpoint {
    pub version: u32,
    pub rev: u64,
}

pub(crate) fn checkpoint(tx: &Transaction<'_>, name: &str) -> Result<Option<Checkpoint>> {
    Ok(tx
        .prepare_cached("SELECT version, rev FROM projection_state WHERE name = ?1")?
        .query_row([name], |r| {
            Ok(Checkpoint {
                version: r.get(0)?,
                rev: u64::try_from(r.get::<_, i64>(1)?).unwrap_or(0),
            })
        })
        .optional()?)
}

pub(crate) fn set_checkpoint(tx: &Transaction<'_>, name: &str, cp: Checkpoint) -> Result<()> {
    tx.prepare_cached(
        "INSERT INTO projection_state (name, version, rev) VALUES (?1, ?2, ?3)
         ON CONFLICT (name) DO UPDATE SET version = excluded.version, rev = excluded.rev",
    )?
    .execute(crate::sql::params![
        name,
        cp.version,
        i64::try_from(cp.rev).unwrap_or(i64::MAX)
    ])?;
    Ok(())
}

pub(crate) fn failed(p: &dyn Projection, rev: u64) -> impl FnOnce(BoxError) -> Error + '_ {
    move |source| Error::Projection {
        name: p.name().to_owned(),
        rev,
        source,
    }
}

/// Brings `p` up to date inside `tx`: rebuilds it if its version changed or it has no
/// checkpoint, otherwise replays what it is missing.
pub(crate) fn sync(tx: &Transaction<'_>, p: &dyn Projection) -> Result<()> {
    match checkpoint(tx, p.name())? {
        Some(cp) if cp.version == p.version() => replay(tx, p, cp.rev),
        _ => rebuild(tx, p),
    }
}

/// Resets `p` and replays the whole log into it, inside `tx`.
pub(crate) fn rebuild(tx: &Transaction<'_>, p: &dyn Projection) -> Result<()> {
    p.reset(tx).map_err(failed(p, 0))?;
    replay(tx, p, 0)
}

/// Applies every event after `from` to `p`, a batch at a time, and records the checkpoint.
pub(crate) fn replay(tx: &Transaction<'_>, p: &dyn Projection, from: u64) -> Result<()> {
    let mut rev = from;
    loop {
        let batch = read_events_after(tx, rev, REPLAY_BATCH)?;
        for event in &batch {
            p.apply(tx, event).map_err(failed(p, event.rev))?;
            rev = event.rev;
        }
        if batch.len() < REPLAY_BATCH {
            break;
        }
    }
    set_checkpoint(
        tx,
        p.name(),
        Checkpoint {
            version: p.version(),
            rev,
        },
    )
}
