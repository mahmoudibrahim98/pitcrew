//! The runner's own SQLite index (not the hub store): one row per transcript, with its cursor.
//!
//! The state directory is private to the user (0700 on Unix) and held by one runner at a time
//! through a lock file. On a network filesystem the index uses a rollback journal, since WAL's
//! shared memory is not safe there.

use crate::derive::Facts;
use crate::fsinfo;
use pitcrew_interfaces::source::{Cursor, SessionMeta};
use pitcrew_protocol::ids::SessionId;
use pitcrew_protocol::model::{Engine, TimestampMs};
use rusqlite::{Connection, OptionalExtension as _, params};
use std::fs;
use std::io;
use std::path::{Path, PathBuf};
use std::time::Duration;

/// Migrations, applied in order; `PRAGMA user_version` counts those applied.
const MIGRATIONS: &[&str] = &[include_str!("../migrations/0001_transcripts.sql")];

const DB_FILE: &str = "runner.sqlite3";
const LOCK_FILE: &str = "runner.lock";

const COLUMNS: &str = "session_id, engine, path, inner_id, cursor, size, mtime, identity, caught_up,
     generation, discovered, emitted_through, meta, facts";

/// Errors from the runner store.
#[derive(Debug, thiserror::Error)]
pub enum StoreError {
    /// SQLite failed.
    #[error("runner store: {0}")]
    Sql(#[from] rusqlite::Error),
    /// A stored value could not be encoded or decoded.
    #[error("runner store value: {0}")]
    Json(#[from] serde_json::Error),
    /// The state directory could not be created or locked.
    #[error("runner state directory: {0}")]
    Io(#[from] io::Error),
    /// A value does not fit its column.
    #[error("runner store value out of range: {0}")]
    Range(String),
    /// Another runner holds this state directory.
    #[error("another runner is using the state directory {}", .0.display())]
    Locked(PathBuf),
    /// The index was written by a newer runner, whose changes this one would not understand.
    #[error("the runner index is at version {found}, newer than this runner knows ({known})")]
    TooNew {
        /// The index's version.
        found: u32,
        /// The newest version this runner knows.
        known: u32,
    },
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
    /// Nothing else changes, so replaying the read after a crash derives the same events (and
    /// ids) as the first time.
    Partial {
        session: SessionId,
        emitted_through: Option<u64>,
    },
    /// A whole read, or a re-index: the full row.
    Full(Box<Row>),
}

#[derive(Debug)]
pub(crate) struct Store {
    conn: Connection,
    /// Held while the store is open; `None` where the filesystem cannot lock.
    _lock: Option<InstanceLock>,
}

impl Store {
    pub fn open(dir: &Path) -> Result<Self, StoreError> {
        create_state_dir(dir)?;
        let lock = InstanceLock::acquire(dir)?;
        let conn = Connection::open(dir.join(DB_FILE))?;
        // Before the first statement that may wait on a lock.
        conn.busy_timeout(Duration::from_secs(5))?;
        if fsinfo::is_network_fs(dir) {
            // WAL's shared-memory index is not safe on network filesystems.
            conn.pragma_update(None, "journal_mode", "DELETE")?;
            conn.pragma_update(None, "synchronous", "FULL")?;
        } else {
            conn.pragma_update(None, "journal_mode", "WAL")?;
            conn.pragma_update(None, "synchronous", "NORMAL")?;
        }
        let mut store = Self { conn, _lock: lock };
        store.migrate()?;
        Ok(store)
    }

    fn migrate(&mut self) -> Result<(), StoreError> {
        let applied: u32 = self
            .conn
            .query_row("PRAGMA user_version", [], |r| r.get(0))?;
        let known = u32::try_from(MIGRATIONS.len()).unwrap_or(u32::MAX);
        if applied > known {
            return Err(StoreError::TooNew {
                found: applied,
                known,
            });
        }
        for (version, sql) in (1u32..).zip(MIGRATIONS).skip(applied as usize) {
            let tx = self.conn.transaction()?;
            tx.execute_batch(sql)?;
            tx.pragma_update(None, "user_version", version)?;
            tx.commit()?;
        }
        Ok(())
    }

    pub fn load_all(&self) -> Result<Vec<Row>, StoreError> {
        let mut stmt = self.conn.prepare(&format!(
            "SELECT {COLUMNS} FROM transcripts ORDER BY path, inner_id"
        ))?;
        let raw = stmt
            .query_map([], raw_row)?
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

    /// The row for a transcript, e.g. one that was deleted and has come back.
    pub fn find(&self, path: &Path, inner_id: Option<&str>) -> Result<Option<Row>, StoreError> {
        let mut stmt = self.conn.prepare_cached(&format!(
            "SELECT {COLUMNS} FROM transcripts WHERE path = ?1 AND inner_id = ?2"
        ))?;
        stmt.query_row(
            params![path_text(path)?, inner_id.unwrap_or("")],
            raw_row,
        )
        .optional()?
        .map(RawRow::decode)
        .transpose()
    }

    /// Moves a row to the transcript's new canonical path (a folder above it became a symlink).
    pub fn set_path(&self, session: SessionId, path: &Path) -> Result<(), StoreError> {
        self.conn.execute(
            "UPDATE transcripts SET path = ?2 WHERE session_id = ?1",
            params![session.0.to_string(), path_text(path)?],
        )?;
        Ok(())
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
            } => {
                self.conn.execute(
                    "UPDATE transcripts SET emitted_through = ?2 WHERE session_id = ?1",
                    params![
                        session.0.to_string(),
                        emitted_through.map(to_i64).transpose()?,
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

/// Creates the state directory. On Unix it is 0700: the index holds session titles and paths.
fn create_state_dir(dir: &Path) -> io::Result<()> {
    #[cfg(unix)]
    {
        use std::os::unix::fs::{DirBuilderExt as _, PermissionsExt as _};
        fs::DirBuilder::new()
            .recursive(true)
            .mode(0o700)
            .create(dir)?;
        let mode = fs::metadata(dir)?.permissions().mode();
        if mode & 0o077 != 0 {
            tracing::warn!(
                dir = %dir.display(),
                mode = format!("{:o}", mode & 0o777),
                "the runner state directory is open to other users"
            );
        }
    }
    #[cfg(not(unix))]
    fs::create_dir_all(dir)?;
    Ok(())
}

/// One runner per state directory: an exclusive lock on a file in it, held until dropped. It
/// uses `flock` on Unix and an unshared open on Windows; both end with the process.
#[derive(Debug)]
struct InstanceLock {
    _file: fs::File,
}

impl InstanceLock {
    /// `Ok(None)` when the filesystem cannot lock (e.g. NFS without a lock daemon): the runner
    /// still starts, without the guard, and says so.
    fn acquire(dir: &Path) -> Result<Option<Self>, StoreError> {
        let path = dir.join(LOCK_FILE);
        let mut options = fs::OpenOptions::new();
        options.read(true).write(true).create(true).truncate(false);
        #[cfg(unix)]
        {
            use std::os::unix::fs::OpenOptionsExt as _;
            options.mode(0o600);
            let file = options.open(&path)?;
            match rustix::fs::flock(&file, rustix::fs::FlockOperation::NonBlockingLockExclusive) {
                Ok(()) => Ok(Some(Self { _file: file })),
                Err(e) if e == rustix::io::Errno::WOULDBLOCK => {
                    Err(StoreError::Locked(dir.to_path_buf()))
                }
                Err(e) => {
                    tracing::warn!(
                        lock = %path.display(),
                        error = %io::Error::from(e),
                        "cannot lock the runner state directory; not guarding against a second runner"
                    );
                    Ok(None)
                }
            }
        }
        #[cfg(windows)]
        {
            use std::os::windows::fs::OpenOptionsExt as _;
            // No sharing: a second open fails with a sharing violation while this one is held.
            const ERROR_SHARING_VIOLATION: i32 = 32;
            options.share_mode(0);
            match options.open(&path) {
                Ok(file) => Ok(Some(Self { _file: file })),
                Err(e) if e.raw_os_error() == Some(ERROR_SHARING_VIOLATION) => {
                    Err(StoreError::Locked(dir.to_path_buf()))
                }
                Err(e) => Err(e.into()),
            }
        }
        #[cfg(not(any(unix, windows)))]
        {
            Ok(Some(Self {
                _file: options.open(&path)?,
            }))
        }
    }
}

fn raw_row(r: &rusqlite::Row<'_>) -> rusqlite::Result<RawRow> {
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
            })
            .expect("partial");
        let got = store.get(r.session).expect("get").expect("row");
        // A partial commit changes nothing but `emitted_through`.
        assert_eq!((got.emitted_through, got.discovered), (Some(40), false));
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
        assert_eq!(reopened.load_all().expect("load"), vec![full.clone()]);

        let found = reopened.find(&full.path, None).expect("find");
        assert_eq!(found.as_ref().map(|r| r.session), Some(full.session));
        assert!(reopened.find(&full.path, Some("x")).expect("find").is_none());
        reopened
            .set_path(full.session, Path::new("/real/a.jsonl"))
            .expect("set path");
        assert!(reopened.find(&full.path, None).expect("find").is_none());
        assert!(
            reopened
                .find(Path::new("/real/a.jsonl"), None)
                .expect("find")
                .is_some()
        );
    }

    #[test]
    fn one_runner_per_state_directory() {
        let dir = tempfile::tempdir().expect("tempdir");
        let first = Store::open(dir.path()).expect("open");
        let second = Store::open(dir.path());
        assert!(
            matches!(second, Err(StoreError::Locked(_))),
            "{second:?}"
        );
        drop(first);
        Store::open(dir.path()).expect("open after the first closed");
    }

    #[test]
    fn an_index_from_a_newer_runner_is_refused() {
        let dir = tempfile::tempdir().expect("tempdir");
        let store = Store::open(dir.path()).expect("open");
        store
            .conn
            .pragma_update(None, "user_version", 99)
            .expect("bump");
        drop(store);
        let err = Store::open(dir.path()).expect_err("too new");
        assert!(
            matches!(err, StoreError::TooNew { found: 99, known: 1 }),
            "{err}"
        );
    }

    #[cfg(unix)]
    #[test]
    fn the_state_directory_is_private() {
        use std::os::unix::fs::PermissionsExt as _;
        let dir = tempfile::tempdir().expect("tempdir");
        let state = dir.path().join("a").join("state");
        drop(Store::open(&state).expect("open"));
        let mode = fs::metadata(&state).expect("meta").permissions().mode() & 0o777;
        assert_eq!(mode, 0o700);
    }
}
