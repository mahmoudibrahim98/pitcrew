//! Forward-only schema migrations.
//!
//! Every `migrations/NNNN_<name>.sql` file is embedded at build time. Number ranges belong to
//! streams (ADR-0004), so files from several streams interleave. The store records what it has
//! applied in `schema_migrations` and applies any known migration that is missing, each in its own
//! IMMEDIATE transaction that re-checks `schema_migrations`, so several processes can open a fresh
//! file at once. Foreign keys are off while a migration runs and checked before it commits.

use crate::error::{DbError, Error, Result};
use crate::scan;
use rusqlite::{Connection, OptionalExtension, TransactionBehavior};
use std::borrow::Cow;
use std::collections::BTreeSet;
use std::path::Path;

/// One migration.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Migration {
    /// Its number; migrations apply in this order.
    pub version: u32,
    /// Its name, from the file name.
    pub name: Cow<'static, str>,
    /// The SQL.
    pub sql: Cow<'static, str>,
}

impl Migration {
    const fn embedded(version: u32, name: &'static str, sql: &'static str) -> Self {
        Self {
            version,
            name: Cow::Borrowed(name),
            sql: Cow::Borrowed(sql),
        }
    }
}

static EMBEDDED: &[Migration] = include!(concat!(env!("OUT_DIR"), "/migrations.rs"));

/// The migrations built into this binary, in order.
#[must_use]
pub fn embedded() -> &'static [Migration] {
    EMBEDDED
}

/// Reads migrations from a directory with the same rules as the build: `NNNN_<name>.sql`, sorted,
/// no duplicate numbers. Other files are ignored. Used by tests and tools.
///
/// # Errors
///
/// [`Error::BadMigrations`] if the directory cannot be read, a SQL file is misnamed, or two files
/// share a number.
pub fn load_dir(dir: &Path) -> Result<Vec<Migration>> {
    let found = scan::scan_dir(dir).map_err(Error::BadMigrations)?;
    found
        .into_iter()
        .map(|f| {
            let sql = std::fs::read_to_string(&f.path)
                .map_err(|e| Error::BadMigrations(format!("{}: {e}", f.path.display())))?;
            Ok(Migration {
                version: f.version,
                name: Cow::Owned(f.name),
                sql: Cow::Owned(sql),
            })
        })
        .collect()
}

/// Applies the missing migrations. Returns the versions applied now.
pub(crate) fn apply(conn: &mut Connection, migrations: &[Migration]) -> Result<Vec<u32>> {
    let mut sorted: Vec<&Migration> = migrations.iter().collect();
    sorted.sort_by_key(|m| m.version);
    for pair in sorted.windows(2) {
        if pair[0].version == pair[1].version {
            return Err(Error::BadMigrations(format!(
                "duplicate migration number {:04}",
                pair[0].version
            )));
        }
    }

    conn.execute_batch(
        "CREATE TABLE IF NOT EXISTS schema_migrations (
           version    INTEGER PRIMARY KEY,
           name       TEXT    NOT NULL,
           applied_at INTEGER NOT NULL
         ) STRICT;",
    )?;

    let applied: BTreeSet<u32> = {
        let mut stmt = conn.prepare("SELECT version FROM schema_migrations")?;
        stmt.query_map([], |row| row.get::<_, u32>(0))?
            .collect::<rusqlite::Result<_>>()?
    };
    let supported = sorted.last().map_or(0, |m| m.version);
    let known: BTreeSet<u32> = sorted.iter().map(|m| m.version).collect();
    if let Some(&found) = applied.last()
        && found > supported
    {
        return Err(Error::SchemaTooNew { found, supported });
    }
    if let Some(&version) = applied.difference(&known).next() {
        return Err(Error::UnknownMigration { version });
    }

    let mut now_applied = Vec::new();
    for m in sorted {
        if applied.contains(&m.version) {
            continue;
        }
        let fail = |source: rusqlite::Error| Error::Migration {
            version: m.version,
            name: m.name.to_string(),
            source: DbError::new(source),
        };
        // SQLite's procedure for schema changes: foreign keys off outside the transaction (the
        // pragma is a no-op inside one), so a table rebuild's DROP TABLE does not cascade or null
        // child rows; check the keys before commit; turn them back on even on error.
        conn.pragma_update(None, "foreign_keys", "OFF")
            .map_err(fail)?;
        let result = apply_one(conn, m);
        let restored = conn.pragma_update(None, "foreign_keys", "ON");
        if result.map_err(fail)? {
            now_applied.push(m.version);
        }
        restored.map_err(fail)?;
    }
    Ok(now_applied)
}

/// Applies one migration unless another process already has. Returns whether it applied it.
/// Foreign keys must be off.
fn apply_one(conn: &mut Connection, m: &Migration) -> rusqlite::Result<bool> {
    // IMMEDIATE, so two processes opening the store serialise here instead of failing with
    // SQLITE_BUSY; the loser then sees the winner's row and skips the migration.
    let tx = conn.transaction_with_behavior(TransactionBehavior::Immediate)?;
    let done = tx
        .query_row(
            "SELECT 1 FROM schema_migrations WHERE version = ?1",
            [m.version],
            |_| Ok(()),
        )
        .optional()?
        .is_some();
    if done {
        return Ok(false);
    }
    tx.execute_batch(&m.sql)?;
    let dangling = tx
        .query_row("PRAGMA foreign_key_check", [], |r| {
            Ok(format!(
                "foreign key check failed: a row in {} (rowid {}) references a missing row in {}",
                r.get::<_, String>(0)?,
                r.get::<_, Option<i64>>(1)?
                    .map_or_else(|| "none".to_owned(), |id| id.to_string()),
                r.get::<_, String>(2)?,
            ))
        })
        .optional()?;
    if let Some(msg) = dangling {
        return Err(rusqlite::Error::SqliteFailure(
            rusqlite::ffi::Error::new(rusqlite::ffi::SQLITE_CONSTRAINT_FOREIGNKEY),
            Some(msg),
        ));
    }
    tx.execute(
        "INSERT INTO schema_migrations (version, name, applied_at) VALUES (?1, ?2, ?3)",
        rusqlite::params![m.version, m.name.as_ref(), now_ms()],
    )?;
    tx.commit()?;
    Ok(true)
}

/// The highest applied version, or `None` for an empty database.
pub(crate) fn current_version(conn: &Connection) -> Result<Option<u32>> {
    Ok(conn
        .query_row("SELECT MAX(version) FROM schema_migrations", [], |row| {
            row.get::<_, Option<u32>>(0)
        })
        .optional()?
        .flatten())
}

fn now_ms() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |d| i64::try_from(d.as_millis()).unwrap_or(i64::MAX))
}
