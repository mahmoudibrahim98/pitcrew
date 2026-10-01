//! The run log as a store projection (`office.runs`, tables from migration `0301`).
//!
//! The office is deterministic given the events, so its run log is a projection: [`RunLog`]
//! replays the rules inside each append and writes one `office_runs` row per action with its
//! outcome. A rebuild (reset, then the whole log) gives the same rows as appending one batch at a
//! time. The office's state is saved in `office_state`, a row per changed key per event, and kept
//! in memory between appends; it is read back only when the stored revision is not the one in
//! memory (a new process, another writer, or a rolled-back append).

use crate::action::{Action, Entry, Outcome};
use crate::office::{Config, Office};
use crate::rule::Rule;
use crate::rules::default_rules;
use pitcrew_protocol::ids::EventId;
use pitcrew_store::sql::{Connection, OptionalExtension, Transaction, params};
use pitcrew_store::{BoxError, Projection, StoredEvent};
use std::sync::{Mutex, MutexGuard};

/// The projection's name.
pub const RUN_LOG: &str = "office.runs";

type RuleFactory = Box<dyn Fn() -> Vec<Box<dyn Rule>> + Send + Sync>;

/// The office's run log, as a projection. Register it with `Store::open_with`.
pub struct RunLog {
    config: Config,
    rules: RuleFactory,
    cache: Mutex<Option<Office>>,
}

impl std::fmt::Debug for RunLog {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("RunLog")
            .field("config", &self.config)
            .finish_non_exhaustive()
    }
}

impl RunLog {
    /// The run log of an office with the default rules.
    #[must_use]
    pub fn new(config: Config) -> Self {
        Self::with_rules(config, default_rules)
    }

    /// The run log of an office with other rules. `rules` makes a fresh set each time the office
    /// is restored.
    #[must_use]
    pub fn with_rules(
        config: Config,
        rules: impl Fn() -> Vec<Box<dyn Rule>> + Send + Sync + 'static,
    ) -> Self {
        Self {
            config,
            rules: Box::new(rules),
            cache: Mutex::new(None),
        }
    }

    fn lock(&self) -> MutexGuard<'_, Option<Office>> {
        match self.cache.lock() {
            Ok(guard) => guard,
            // A panic mid-apply may have left the office half-changed: read it back.
            Err(poisoned) => {
                let mut guard = poisoned.into_inner();
                *guard = None;
                guard
            }
        }
    }

    fn restore(&self, tx: &Transaction<'_>) -> Result<Office, BoxError> {
        let mut stmt = tx.prepare("SELECT key, value FROM office_state ORDER BY key")?;
        let rows: Vec<(String, String)> = stmt
            .query_map([], |r| Ok((r.get(0)?, r.get(1)?)))?
            .collect::<Result<_, _>>()?;
        let office = Office::restore(
            self.config.clone(),
            (self.rules)(),
            rows.iter().map(|(k, v)| (k.as_str(), v.as_str())),
        )?;
        Ok(office)
    }
}

fn stored_rev(tx: &Transaction<'_>) -> Result<u64, BoxError> {
    let meta: Option<String> = tx
        .prepare_cached("SELECT value FROM office_state WHERE key = 'meta'")?
        .query_row([], |r| r.get(0))
        .optional()?;
    #[derive(serde::Deserialize)]
    struct Rev {
        rev: u64,
    }
    Ok(match meta {
        Some(json) => serde_json::from_str::<Rev>(&json)?.rev,
        None => 0,
    })
}

fn to_i64(n: u64) -> i64 {
    i64::try_from(n).unwrap_or(i64::MAX)
}

impl Projection for RunLog {
    fn name(&self) -> &str {
        RUN_LOG
    }

    fn version(&self) -> u32 {
        1
    }

    fn reset(&self, tx: &Transaction<'_>) -> Result<(), BoxError> {
        tx.execute_batch("DELETE FROM office_runs; DELETE FROM office_state;")?;
        *self.lock() = Some(Office::with_rules(self.config.clone(), (self.rules)()));
        Ok(())
    }

    fn apply(&self, tx: &Transaction<'_>, event: &StoredEvent) -> Result<(), BoxError> {
        let mut cache = self.lock();
        let stored = stored_rev(tx)?;
        let office = match cache.take() {
            Some(office) if office.rev() == stored => office,
            _ => self.restore(tx)?,
        };
        // If anything below fails, the append rolls back, the stored revision no longer matches
        // the office's, and the next apply reads the state back.
        let office = cache.insert(office);
        let entries = office.on_event(event.rev, &event.event);
        let mut insert = tx.prepare_cached(
            "INSERT INTO office_runs (rev, seq, event, at, rule, action, outcome, reason)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8)",
        )?;
        for e in &entries {
            insert.execute(params![
                to_i64(e.rev),
                e.seq,
                e.event.0.to_string(),
                e.at,
                e.rule,
                serde_json::to_string(&e.action)?,
                e.outcome.code(),
                e.outcome.reason(),
            ])?;
        }
        let mut upsert = tx.prepare_cached(
            "INSERT INTO office_state (key, value) VALUES (?1, ?2)
             ON CONFLICT (key) DO UPDATE SET value = excluded.value",
        )?;
        let mut delete = tx.prepare_cached("DELETE FROM office_state WHERE key = ?1")?;
        for row in office.take_changes()? {
            match &row.value {
                Some(value) => upsert.execute(params![row.key, value])?,
                None => delete.execute(params![row.key])?,
            };
        }
        Ok(())
    }
}

/// A run-log row that does not read back.
#[derive(Debug, thiserror::Error)]
#[non_exhaustive]
pub enum ReadError {
    /// The query failed.
    #[error(transparent)]
    Sql(#[from] pitcrew_store::sql::Error),
    /// A stored action is not valid JSON for an [`Action`].
    #[error("run log action does not read: {0}")]
    Action(#[from] serde_json::Error),
    /// A stored event id, revision or outcome is malformed.
    #[error("run log row {0} is malformed")]
    Row(i64),
}

/// The run log after revision `after`, oldest first, at most `limit` entries. Use it inside
/// `Store::read`.
///
/// # Errors
///
/// [`ReadError`] when the query fails or a row does not read back.
pub fn read_runs(conn: &Connection, after: u64, limit: usize) -> Result<Vec<Entry>, ReadError> {
    let mut stmt = conn.prepare_cached(
        "SELECT rev, seq, event, at, rule, action, outcome, reason FROM office_runs
         WHERE rev > ?1 ORDER BY rev, seq LIMIT ?2",
    )?;
    let rows = stmt.query_map(
        params![to_i64(after), i64::try_from(limit).unwrap_or(i64::MAX)],
        |r| {
            Ok((
                r.get::<_, i64>(0)?,
                r.get::<_, u32>(1)?,
                r.get::<_, String>(2)?,
                r.get::<_, i64>(3)?,
                r.get::<_, String>(4)?,
                r.get::<_, String>(5)?,
                r.get::<_, String>(6)?,
                r.get::<_, Option<String>>(7)?,
            ))
        },
    )?;
    let mut out = Vec::new();
    for row in rows {
        let (rev, seq, event, at, rule, action, outcome, reason) = row?;
        let malformed = || ReadError::Row(rev);
        out.push(Entry {
            rev: u64::try_from(rev).map_err(|_| malformed())?,
            seq,
            event: event.parse::<EventId>().map_err(|_| malformed())?,
            at,
            rule,
            action: serde_json::from_str::<Action>(&action)?,
            outcome: Outcome::from_parts(&outcome, reason.as_deref()).ok_or_else(malformed)?,
        });
    }
    Ok(out)
}
