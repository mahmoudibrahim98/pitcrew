//! The runner's own SQLite index (not the hub store): one row per transcript, with its cursor.

use crate::derive::Facts;
use pitcrew_interfaces::source::{Cursor, SessionMeta};
use pitcrew_protocol::ids::SessionId;
use pitcrew_protocol::model::{Engine, TimestampMs};
use rusqlite::{Connection, params};
use std::path::{Path, PathBuf};

/// Migrations, applied in order; `PRAGMA user_version` counts those applied.
const MIGRATIONS: &[&str] = &[include_str!("../migrations/0001_transcripts.sql")];

/// Errors from the runner store.
#[derive(Debug, thiserror::Error)]
pub enum StoreError {
    /// SQLite failed.
    #[error("runner store: {0}")]
    Sql(#[from] rusqlite::Error),
    /// A stored value could not be encoded or decoded.
    #[error("runner store value: {0}")]
    Json(#[from] serde_json::Error),
    /// The state directory could not be created.
    #[error("runner state directory: {0}")]
    Io(#[from] std::io::Error),
    /// A value does not fit its column.
    #[error("runner store value out of range: {0}")]
    Range(String),
}

/// A transcript row.
#[derive(Clone, Debug, PartialEq)]
pub(crate) struct Row {
    pub session: SessionId,
    pub engine: Engine,
    pub path: PathBuf,
    pub inner_id: Option<String>,
    pub cursor: Cursor,
    pub size: u64,
    pub mtime: TimestampMs,
    pub identity: Option<String>,
    /// The cursor had reached the end of the file at size/mtime.
    pub caught_up: bool,
    pub generation: u32,
    pub discovered: bool,
    pub emitted_through: Option<u64>,
    pub meta: Option<SessionMeta>,
    pub facts: Facts,
}

/// What to save once the sink has accepted a batch.
#[derive(Clone, Debug, PartialEq)]
pub(crate) enum Commit {
    /// Part of a read: the events up to `emitted_through` are accepted, the cursor not yet.
    Partial {
        session: SessionId,
        emitted_through: Option<u64>,
        discovered: bool,
    },
    /// A whole read, or a re-index: the full row.
    Full(Box<Row>),
}

#[derive(Debug)]
pub(crate) struct Store {
    conn: Connection,
}

impl Store {
    pub fn open(dir: &Path) -> Result<Self, StoreError> {
        std::fs::create_dir_all(dir)?;
        let conn = Connection::open(dir.join("runner.sqlite3"))?;
        conn.pragma_update(None, "journal_mode", "WAL")?;
        conn.pragma_update(None, "synchronous", "NORMAL")?;
        conn.busy_timeout(std::time::Duration::from_secs(5))?;
        let mut store = Self { conn };
        store.migrate()?;
        Ok(store)
    }

    fn migrate(&mut self) -> Result<(), StoreError> {
        let applied: u32 = self
            .conn
            .query_row("PRAGMA user_version", [], |r| r.get(0))?;
        for (version, sql) in (1u32..).zip(MIGRATIONS).skip(applied as usize) {
            let tx = self.conn.transaction()?;
            tx.execute_batch(sql)?;
            tx.pragma_update(None, "user_version", version)?;
            tx.commit()?;
        }
        Ok(())
    }

    pub fn load_all(&self) -> Result<Vec<Row>, StoreError> {
        let mut stmt = self.conn.prepare(
            "SELECT session_id, engine, path, inner_id, cursor, size, mtime, identity, caught_up,
                    generation, discovered, emitted_through, meta, facts
             FROM transcripts ORDER BY path, inner_id",
        )?;
        let raw = stmt
            .query_map([], |r| {
                Ok(RawRow {
                    session: r.get(0)?,
                    engine: r.get(1)?,
                    path: r.get(2)?,
                    inner_id: r.get(3)?,
                    cursor: r.get(4)?,
                    size: r.get(5)?,
                    mtime: r.get(6)?,
                    identity: r.get(7)?,
                    caught_up: r.get(8)?,
                    generation: r.get(9)?,
                    discovered: r.get(10)?,
                    emitted_through: r.get(11)?,
                    meta: r.get(12)?,
                    facts: r.get(13)?,
                })
            })?
            .collect::<Result<Vec<_>, _>>()?;
        let mut rows = Vec::with_capacity(raw.len());
        for r in raw {
            match r.decode() {
                Ok(row) => rows.push(row),
                // One bad row must not stop the runner. Its path stays taken, so that transcript is
                // not indexed again until the row is removed.
                Err(e) => tracing::error!(error = %e, "skipping an unreadable runner store row"),
            }
        }
        Ok(rows)
    }

    /// Records a newly discovered transcript, before anything is read from it, so its session id
    /// survives a crash.
    pub fn insert(&self, row: &Row) -> Result<(), StoreError> {
        self.conn.execute(
            "INSERT INTO transcripts (session_id, engine, path, inner_id, cursor)
             VALUES (?1, ?2, ?3, ?4, ?5)",
            params![
                row.session.0.to_string(),
                engine_text(row.engine)?,
                path_text(&row.path)?,
                row.inner_id.as_deref().unwrap_or(""),
                serde_json::to_string(&row.cursor)?,
            ],
        )?;
        Ok(())
    }

    pub fn commit(&self, commit: &Commit) -> Result<(), StoreError> {
        match commit {
            Commit::Partial {
                session,
                emitted_through,
                discovered,
            } => {
                self.conn.execute(
                    "UPDATE transcripts SET emitted_through = ?2, discovered = discovered OR ?3
                     WHERE session_id = ?1",
                    params![
                        session.0.to_string(),
                        emitted_through.map(to_i64).transpose()?,
                        discovered
                    ],
                )?;
            }
            Commit::Full(row) => {
                self.conn.execute(
                    "UPDATE transcripts SET cursor = ?2, size = ?3, mtime = ?4, identity = ?5,
                        caught_up = ?6, generation = ?7, discovered = ?8, emitted_through = ?9,
                        meta = ?10, facts = ?11
                     WHERE session_id = ?1",
                    params![
                        row.session.0.to_string(),
                        serde_json::to_string(&row.cursor)?,
                        to_i64(row.size)?,
                        row.mtime,
                        row.identity,
                        row.caught_up,
                        row.generation,
                        row.discovered,
                        row.emitted_through.map(to_i64).transpose()?,
                        row.meta.as_ref().map(serde_json::to_string).transpose()?,
                        serde_json::to_string(&row.facts)?,
                    ],
                )?;
            }
        }
        Ok(())
    }

    #[cfg(test)]
    pub fn get(&self, session: SessionId) -> Result<Option<Row>, StoreError> {
        Ok(self.load_all()?.into_iter().find(|r| r.session == session))
    }
}

struct RawRow {
    session: String,
    engine: String,
    path: String,
    inner_id: String,
    cursor: String,
    size: i64,
    mtime: i64,
    identity: Option<String>,
    caught_up: bool,
    generation: u32,
    discovered: bool,
    emitted_through: Option<i64>,
    meta: Option<String>,
    facts: Option<String>,
}

impl RawRow {
    fn decode(self) -> Result<Row, StoreError> {
        let session = self
            .session
            .parse()
            .map_err(|_| StoreError::Range(format!("session id {:?}", self.session)))?;
        Ok(Row {
            session,
            engine: serde_json::from_value(serde_json::Value::String(self.engine))?,
            path: PathBuf::from(self.path),
            inner_id: Some(self.inner_id).filter(|s| !s.is_empty()),
            cursor: serde_json::from_str(&self.cursor)?,
            size: u64::try_from(self.size).map_err(|_| StoreError::Range("size".into()))?,
            mtime: self.mtime,
            identity: self.identity,
            caught_up: self.caught_up,
            generation: self.generation,
            discovered: self.discovered,
            emitted_through: self
                .emitted_through
                .map(|v| u64::try_from(v).map_err(|_| StoreError::Range("offset".into())))
                .transpose()?,
            meta: self.meta.as_deref().map(serde_json::from_str).transpose()?,
            facts: self
                .facts
                .as_deref()
                .map(serde_json::from_str)
                .transpose()?
                .unwrap_or_default(),
        })
    }
}

fn to_i64(v: u64) -> Result<i64, StoreError> {
    i64::try_from(v).map_err(|_| StoreError::Range(v.to_string()))
}

fn engine_text(engine: Engine) -> Result<String, StoreError> {
    match serde_json::to_value(engine)? {
        serde_json::Value::String(s) => Ok(s),
        other => Err(StoreError::Range(other.to_string())),
    }
}

/// Paths are stored as text; a path that is not valid Unicode is not indexed.
pub(crate) fn path_text(path: &Path) -> Result<&str, StoreError> {
    path.to_str()
        .ok_or_else(|| StoreError::Range(format!("non-Unicode path {}", path.display())))
}

#[cfg(test)]
mod tests {
    use super::*;
    use pitcrew_protocol::model::SessionState;

    fn row(path: &str) -> Row {
        Row {
            session: SessionId::new(),
            engine: Engine::Claude,
            path: path.into(),
            inner_id: None,
            cursor: Cursor::default(),
            size: 0,
            mtime: 0,
            identity: None,
            caught_up: false,
            generation: 0,
            discovered: false,
            emitted_through: None,
            meta: None,
            facts: Facts::default(),
        }
    }

    #[test]
    fn rows_round_trip_and_commits_apply() {
        let dir = tempfile::tempdir().expect("tempdir");
        let store = Store::open(dir.path()).expect("open");
        let r = row("/t/a.jsonl");
        store.insert(&r).expect("insert");
        assert!(store.insert(&row("/t/a.jsonl")).is_err(), "path is unique");

        store
            .commit(&Commit::Partial {
                session: r.session,
                emitted_through: Some(40),
                discovered: true,
            })
            .expect("partial");
        let got = store.get(r.session).expect("get").expect("row");
        assert_eq!((got.emitted_through, got.discovered), (Some(40), true));
        assert_eq!(got.cursor, Cursor::default());

        let mut full = got.clone();
        full.cursor = Cursor {
            offset: 99,
            state: Some(serde_json::json!({"k": 1})),
        };
        full.size = 120;
        full.identity = Some("1:2".into());
        full.facts.state = SessionState::Idle;
        full.meta = Some(SessionMeta {
            native_id: "n".into(),
            ..SessionMeta::default()
        });
        store
            .commit(&Commit::Full(Box::new(full.clone())))
            .expect("full");
        drop(store);

        let reopened = Store::open(dir.path()).expect("reopen");
        assert_eq!(reopened.load_all().expect("load"), vec![full]);
    }
}
