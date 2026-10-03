//! The runner's own SQLite index (not the hub store): one row per transcript, with its cursor;
//! the terminals the runner started; and the outcomes of hub commands.
//!
//! The state directory is private to the user (0700 on Unix) and held by one runner at a time
//! through a lock file. On a network filesystem the index uses a rollback journal, since WAL's
//! shared memory is not safe there.

use crate::derive::{Facts, Parent};
use crate::fsinfo;
use pitcrew_interfaces::source::{Cursor, SessionMeta};
use pitcrew_protocol::ids::{CommandId, SessionId, TerminalId};
use pitcrew_protocol::model::{Engine, TimestampMs};
use pitcrew_protocol::runner::CommandOutcome;
use rusqlite::{Connection, OptionalExtension as _, params};
use std::collections::HashSet;
use std::fs;
use std::io;
use std::path::{Path, PathBuf};
use std::time::Duration;

/// Migrations, applied in order; `PRAGMA user_version` counts those applied.
const MIGRATIONS: &[&str] = &[
    include_str!("../migrations/0001_transcripts.sql"),
    include_str!("../migrations/0002_links_and_commands.sql"),
];

const DB_FILE: &str = "runner.sqlite3";
const LOCK_FILE: &str = "runner.lock";

const COLUMNS: &str = "session_id, engine, path, inner_id, cursor, size, mtime, identity, \
                       caught_up, generation, discovered, meta, facts";
const TERMINAL_COLUMNS: &str =
    "terminal_id, native_target, session_id, engine, native_id, cwd, started_at";

/// A terminal started for a CLI whose session id is not known in advance is claimed by a session
/// in its folder that starts within this long.
const CLAIM_WINDOW_MS: TimestampMs = 15 * 60 * 1000;
/// Clock slack when matching a session's start to its terminal's.
const CLAIM_SLACK_MS: TimestampMs = 5_000;
/// Command outcomes are kept this long.
const OUTCOME_TTL_MS: TimestampMs = 7 * 24 * 60 * 60 * 1000;

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
    /// Keys of items whose events were accepted after the cursor: a replay skips them.
    pub accepted: HashSet<u64>,
    pub meta: Option<SessionMeta>,
    pub facts: Facts,
}

/// What the watcher keeps of a transcript row from the start: what tells a change, and what
/// routes hooks to its session. The rest of the row is read when needed ([`Store::load`]).
#[derive(Clone, Debug, PartialEq)]
pub(crate) struct Indexed {
    pub session: SessionId,
    pub engine: Engine,
    pub path: PathBuf,
    pub inner_id: Option<String>,
    pub size: u64,
    pub mtime: TimestampMs,
    pub identity: Option<String>,
    pub caught_up: bool,
    pub discovered: bool,
    /// The CLI's id for the session (see `watch::native_id`), once a read learned its metadata.
    pub native: Option<String>,
    pub subagent: bool,
    /// The parent kept with the row (`Facts::parent`).
    pub parent: Option<Parent>,
}

impl Indexed {
    /// What the watcher keeps of `row`.
    pub fn of(row: &Row) -> Self {
        Self {
            session: row.session,
            engine: row.engine,
            path: row.path.clone(),
            inner_id: row.inner_id.clone(),
            size: row.size,
            mtime: row.mtime,
            identity: row.identity.clone(),
            caught_up: row.caught_up,
            discovered: row.discovered,
            native: row.meta.as_ref().map(|_| native_id(row)),
            subagent: row.meta.as_ref().is_some_and(|m| m.is_subagent),
            parent: row.facts.parent,
        }
    }
}

/// The CLI's id for the session: from its records, else the store's inner id, else the file name.
pub(crate) fn native_id(row: &Row) -> String {
    row.meta
        .as_ref()
        .map(|m| m.native_id.clone())
        .filter(|n| !n.is_empty())
        .or_else(|| row.inner_id.clone())
        .or_else(|| {
            row.path
                .file_stem()
                .map(|s| s.to_string_lossy().into_owned())
        })
        .unwrap_or_default()
}

/// What to save once the sink has accepted a batch.
#[derive(Clone, Debug, PartialEq)]
pub(crate) enum Commit {
    /// Part of a read: the events of these items are accepted, the cursor not yet. Nothing else
    /// changes, so replaying the read after a crash derives the same events (and ids) as the
    /// first time, and skips these items.
    Partial { session: SessionId, keys: Vec<u64> },
    /// A whole read, a re-index, or a reported state: the full row.
    Full(Box<Row>),
}

/// A terminal the runner started.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct TerminalRow {
    pub terminal: TerminalId,
    pub native_target: Option<String>,
    /// The session it runs, once known.
    pub session: Option<SessionId>,
    /// The CLI started in it; `None` for a terminal linked by hand.
    pub engine: Option<Engine>,
    /// The CLI's session id, when it was chosen (or known) at the start.
    pub native_id: Option<String>,
    pub cwd: String,
    pub started_at: TimestampMs,
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

    /// What the watcher keeps of every transcript at start ([`Indexed`]). Each row is read
    /// whole, as before, and let go once its [`Indexed`] is taken: the cursors, facts and metadata
    /// are read again with [`Store::load`] when needed.
    pub fn load_index(&self) -> Result<Vec<Indexed>, StoreError> {
        let mut stmt = self.conn.prepare(&format!(
            "SELECT {COLUMNS} FROM transcripts ORDER BY path, inner_id"
        ))?;
        let mut found = stmt.query([])?;
        let mut rows = Vec::new();
        while let Some(r) = found.next()? {
            // The accepted items do not matter here: `load` reads them with the row.
            match raw_row(r)?.decode(HashSet::new()) {
                Ok(row) => rows.push(Indexed::of(&row)),
                // One bad row must not stop the runner. Its path stays taken, so that transcript is
                // not indexed again until the row is removed.
                Err(e) => tracing::error!(error = %e, "skipping an unreadable runner store row"),
            }
        }
        Ok(rows)
    }

    /// A transcript's whole row, by its session.
    pub fn load(&self, session: SessionId) -> Result<Option<Row>, StoreError> {
        let id = session.0.to_string();
        let mut stmt = self.conn.prepare_cached(&format!(
            "SELECT {COLUMNS} FROM transcripts WHERE session_id = ?1"
        ))?;
        let Some(raw) = stmt.query_row([&id], raw_row).optional()? else {
            return Ok(None);
        };
        let keys = self.accepted_keys(&id)?;
        raw.decode(keys).map(Some)
    }

    #[cfg(test)]
    pub fn load_all(&self) -> Result<Vec<Row>, StoreError> {
        let mut accepted: std::collections::HashMap<String, HashSet<u64>> =
            std::collections::HashMap::new();
        {
            let mut stmt = self
                .conn
                .prepare("SELECT session_id, item_key FROM accepted_items")?;
            let keys = stmt.query_map([], |r| Ok((r.get::<_, String>(0)?, r.get::<_, i64>(1)?)))?;
            for k in keys {
                let (session, key) = k?;
                accepted.entry(session).or_default().insert(from_i64(key));
            }
        }
        let mut stmt = self.conn.prepare(&format!(
            "SELECT {COLUMNS} FROM transcripts ORDER BY path, inner_id"
        ))?;
        let raw = stmt
            .query_map([], raw_row)?
            .collect::<Result<Vec<_>, _>>()?;
        let mut rows = Vec::with_capacity(raw.len());
        for r in raw {
            let keys = accepted.remove(&r.session).unwrap_or_default();
            match r.decode(keys) {
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
        let Some(raw) = stmt
            .query_row(params![path_text(path)?, inner_id.unwrap_or("")], raw_row)
            .optional()?
        else {
            return Ok(None);
        };
        let keys = self.accepted_keys(&raw.session)?;
        raw.decode(keys).map(Some)
    }

    fn accepted_keys(&self, session: &str) -> Result<HashSet<u64>, StoreError> {
        let mut stmt = self
            .conn
            .prepare_cached("SELECT item_key FROM accepted_items WHERE session_id = ?1")?;
        let keys = stmt
            .query_map([session], |r| r.get::<_, i64>(0))?
            .map(|k| k.map(from_i64))
            .collect::<Result<_, _>>()?;
        Ok(keys)
    }

    /// Whether the index has a row for `session`, whether or not its transcript is still there.
    pub fn has_session(&self, session: SessionId) -> Result<bool, StoreError> {
        let found: Option<i64> = self
            .conn
            .prepare_cached("SELECT 1 FROM transcripts WHERE session_id = ?1")?
            .query_row([session.0.to_string()], |r| r.get(0))
            .optional()?;
        Ok(found.is_some())
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
        let tx = self.conn.unchecked_transaction()?;
        match commit {
            Commit::Partial { session, keys } => {
                let mut insert = tx.prepare_cached(
                    "INSERT OR IGNORE INTO accepted_items (session_id, item_key) VALUES (?1, ?2)",
                )?;
                let session = session.0.to_string();
                for k in keys {
                    insert.execute(params![session, to_i64_bits(*k)])?;
                }
            }
            Commit::Full(row) => {
                let session = row.session.0.to_string();
                // `emitted_through` (0001) is no longer used; it is cleared as rows are saved.
                tx.execute(
                    "UPDATE transcripts SET cursor = ?2, size = ?3, mtime = ?4, identity = ?5,
                        caught_up = ?6, generation = ?7, discovered = ?8, emitted_through = NULL,
                        meta = ?9, facts = ?10
                     WHERE session_id = ?1",
                    params![
                        session,
                        serde_json::to_string(&row.cursor)?,
                        to_i64(row.size)?,
                        row.mtime,
                        row.identity,
                        row.caught_up,
                        row.generation,
                        row.discovered,
                        row.meta.as_ref().map(serde_json::to_string).transpose()?,
                        serde_json::to_string(&row.facts)?,
                    ],
                )?;
                // The cursor now covers every accepted item.
                tx.execute(
                    "DELETE FROM accepted_items WHERE session_id = ?1",
                    [&session],
                )?;
            }
        }
        tx.commit()?;
        Ok(())
    }

    /// The session whose transcript names itself `native_id`, if one is indexed. Sub-agents are
    /// not sessions one resumes: one named like a session is never taken for it.
    pub fn session_by_native(
        &self,
        engine: Engine,
        native_id: &str,
    ) -> Result<Option<SessionId>, StoreError> {
        let id: Option<String> = self
            .conn
            .query_row(
                "SELECT session_id FROM transcripts
                 WHERE engine = ?1 AND json_extract(meta, '$.native_id') = ?2
                   AND NOT COALESCE(json_extract(meta, '$.is_subagent'), 0)
                 ORDER BY mtime DESC LIMIT 1",
                params![engine_text(engine)?, native_id],
                |r| r.get(0),
            )
            .optional()?;
        id.map(|s| parse_id(&s)).transpose()
    }

    // ─── Terminals ──────────────────────────────────────────────────────────────────────────

    /// Saves a terminal. A session runs in one terminal at a time: linking it here forgets any
    /// other terminal it had.
    pub fn put_terminal(&self, t: &TerminalRow) -> Result<(), StoreError> {
        let tx = self.conn.unchecked_transaction()?;
        let session = t.session.map(|s| s.0.to_string());
        let terminal = t.terminal.0.to_string();
        if let Some(s) = &session {
            tx.execute(
                "DELETE FROM terminals WHERE session_id = ?1 AND terminal_id != ?2",
                params![s, terminal],
            )?;
        }
        tx.execute(
            "INSERT OR REPLACE INTO terminals
                (terminal_id, native_target, session_id, engine, native_id, cwd, started_at)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7)",
            params![
                terminal,
                t.native_target,
                session,
                t.engine.map(engine_text).transpose()?,
                t.native_id,
                t.cwd,
                t.started_at,
            ],
        )?;
        tx.commit()?;
        Ok(())
    }

    pub fn terminal_of(&self, session: SessionId) -> Result<Option<TerminalRow>, StoreError> {
        let mut stmt = self.conn.prepare_cached(&format!(
            "SELECT {TERMINAL_COLUMNS} FROM terminals WHERE session_id = ?1"
        ))?;
        stmt.query_row([session.0.to_string()], raw_terminal)
            .optional()?
            .map(RawTerminal::decode)
            .transpose()
    }

    pub fn terminals(&self) -> Result<Vec<TerminalRow>, StoreError> {
        let mut stmt = self
            .conn
            .prepare(&format!("SELECT {TERMINAL_COLUMNS} FROM terminals"))?;
        let raw = stmt
            .query_map([], raw_terminal)?
            .collect::<Result<Vec<_>, _>>()?;
        raw.into_iter().map(RawTerminal::decode).collect()
    }

    pub fn forget_terminal(&self, terminal: TerminalId) -> Result<(), StoreError> {
        self.conn.execute(
            "DELETE FROM terminals WHERE terminal_id = ?1",
            [terminal.0.to_string()],
        )?;
        Ok(())
    }

    pub fn unlink_session(&self, session: SessionId) -> Result<(), StoreError> {
        self.conn.execute(
            "DELETE FROM terminals WHERE session_id = ?1",
            [session.0.to_string()],
        )?;
        Ok(())
    }

    /// The terminal a newly discovered session runs in: the one already linked to it (a replay),
    /// else a started terminal waiting for it, matched by the CLI's session id or, for a CLI whose
    /// id is not chosen in advance, by folder and start time.
    pub fn claim_terminal(
        &self,
        session: SessionId,
        engine: Engine,
        native_id: &str,
        cwd: Option<&str>,
        started: TimestampMs,
        now: TimestampMs,
    ) -> Result<Option<TerminalId>, StoreError> {
        if let Some(t) = self.terminal_of(session)? {
            return Ok(Some(t.terminal));
        }
        let mut stmt = self.conn.prepare_cached(&format!(
            "SELECT {TERMINAL_COLUMNS} FROM terminals
             WHERE session_id IS NULL AND engine = ?1 ORDER BY started_at"
        ))?;
        let waiting = stmt
            .query_map([engine_text(engine)?], raw_terminal)?
            .collect::<Result<Vec<_>, _>>()?
            .into_iter()
            .map(RawTerminal::decode)
            .collect::<Result<Vec<_>, _>>()?;
        let by_id = waiting
            .iter()
            .find(|t| !native_id.is_empty() && t.native_id.as_deref() == Some(native_id));
        let by_folder = || {
            waiting.iter().find(|t| {
                t.native_id.is_none()
                    && cwd.is_some_and(|c| same_dir(c, &t.cwd))
                    && t.started_at >= now.saturating_sub(CLAIM_WINDOW_MS)
                    && started >= t.started_at.saturating_sub(CLAIM_SLACK_MS)
            })
        };
        let Some(t) = by_id.or_else(by_folder) else {
            return Ok(None);
        };
        self.conn.execute(
            "UPDATE terminals SET session_id = ?1 WHERE terminal_id = ?2",
            params![session.0.to_string(), t.terminal.0.to_string()],
        )?;
        Ok(Some(t.terminal))
    }

    // ─── Commands ───────────────────────────────────────────────────────────────────────────

    pub fn outcome(&self, command: CommandId) -> Result<Option<CommandOutcome>, StoreError> {
        let text: Option<String> = self
            .conn
            .query_row(
                "SELECT outcome FROM commands WHERE command_id = ?1",
                [command.0.to_string()],
                |r| r.get(0),
            )
            .optional()?;
        Ok(text.map(|t| serde_json::from_str(&t)).transpose()?)
    }

    /// Saves an outcome, and forgets those older than a week.
    pub fn save_outcome(
        &self,
        command: CommandId,
        outcome: &CommandOutcome,
        now: TimestampMs,
    ) -> Result<(), StoreError> {
        let tx = self.conn.unchecked_transaction()?;
        tx.execute(
            "INSERT OR REPLACE INTO commands (command_id, outcome, at) VALUES (?1, ?2, ?3)",
            params![command.0.to_string(), serde_json::to_string(outcome)?, now],
        )?;
        tx.execute(
            "DELETE FROM commands WHERE at < ?1",
            [now.saturating_sub(OUTCOME_TTL_MS)],
        )?;
        tx.commit()?;
        Ok(())
    }

    /// Hides the transcripts table (or brings it back), so lookups in it fail.
    #[cfg(test)]
    pub fn hide_transcripts(&self, hidden: bool) -> Result<(), StoreError> {
        self.conn.execute_batch(if hidden {
            "ALTER TABLE transcripts RENAME TO transcripts_hidden"
        } else {
            "ALTER TABLE transcripts_hidden RENAME TO transcripts"
        })?;
        Ok(())
    }

    #[cfg(test)]
    pub fn get(&self, session: SessionId) -> Result<Option<Row>, StoreError> {
        let found = self.load(session)?;
        assert_eq!(
            found,
            self.load_all()?.into_iter().find(|r| r.session == session),
            "a row loaded alone is the row loaded with the others"
        );
        Ok(found)
    }
}

/// Whether two folder paths name the same folder, ignoring trailing separators.
fn same_dir(a: &str, b: &str) -> bool {
    let trim = |s: &str| s.trim_end_matches(['/', '\\']).to_owned();
    trim(a) == trim(b)
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

#[cfg(unix)]
impl Drop for InstanceLock {
    /// Unlocks before the file is closed. A process another thread is starting holds a copy of
    /// every descriptor until it runs its program, and an flock lasts while any copy is open:
    /// closing alone can leave the lock held a moment after the runner stopped, and refuse the
    /// next one (seen on macOS, where reading the host name starts `hostname`). `LOCK_UN`
    /// releases the lock of the open file every copy shares, so a child forked to keep the lock
    /// would lose it here; none is meant to.
    fn drop(&mut self) {
        let _ = rustix::fs::flock(&self._file, rustix::fs::FlockOperation::Unlock);
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
        meta: r.get(11)?,
        facts: r.get(12)?,
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
    meta: Option<String>,
    facts: Option<String>,
}

impl RawRow {
    fn decode(self, accepted: HashSet<u64>) -> Result<Row, StoreError> {
        Ok(Row {
            session: parse_id(&self.session)?,
            engine: engine_from(self.engine)?,
            path: PathBuf::from(self.path),
            inner_id: Some(self.inner_id).filter(|s| !s.is_empty()),
            cursor: serde_json::from_str(&self.cursor)?,
            size: u64::try_from(self.size).map_err(|_| StoreError::Range("size".into()))?,
            mtime: self.mtime,
            identity: self.identity,
            caught_up: self.caught_up,
            generation: self.generation,
            discovered: self.discovered,
            accepted,
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

fn raw_terminal(r: &rusqlite::Row<'_>) -> rusqlite::Result<RawTerminal> {
    Ok(RawTerminal {
        terminal: r.get(0)?,
        native_target: r.get(1)?,
        session: r.get(2)?,
        engine: r.get(3)?,
        native_id: r.get(4)?,
        cwd: r.get(5)?,
        started_at: r.get(6)?,
    })
}

struct RawTerminal {
    terminal: String,
    native_target: Option<String>,
    session: Option<String>,
    engine: Option<String>,
    native_id: Option<String>,
    cwd: String,
    started_at: i64,
}

impl RawTerminal {
    fn decode(self) -> Result<TerminalRow, StoreError> {
        Ok(TerminalRow {
            terminal: parse_id(&self.terminal)?,
            native_target: self.native_target,
            session: self.session.as_deref().map(parse_id).transpose()?,
            engine: self.engine.map(engine_from).transpose()?,
            native_id: self.native_id,
            cwd: self.cwd,
            started_at: self.started_at,
        })
    }
}

fn parse_id<T: std::str::FromStr>(s: &str) -> Result<T, StoreError> {
    s.parse()
        .map_err(|_| StoreError::Range(format!("id {s:?}")))
}

fn to_i64(v: u64) -> Result<i64, StoreError> {
    i64::try_from(v).map_err(|_| StoreError::Range(v.to_string()))
}

/// A `u64` key in an `INTEGER` column, bit for bit.
fn to_i64_bits(v: u64) -> i64 {
    i64::from_ne_bytes(v.to_ne_bytes())
}

fn from_i64(v: i64) -> u64 {
    u64::from_ne_bytes(v.to_ne_bytes())
}

fn engine_text(engine: Engine) -> Result<String, StoreError> {
    match serde_json::to_value(engine)? {
        serde_json::Value::String(s) => Ok(s),
        other => Err(StoreError::Range(other.to_string())),
    }
}

fn engine_from(text: String) -> Result<Engine, StoreError> {
    Ok(serde_json::from_value(serde_json::Value::String(text))?)
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
            accepted: HashSet::new(),
            meta: None,
            facts: Facts::default(),
        }
    }

    /// A sub-agent named like a session, even a newer one, is never the session to resume.
    #[test]
    fn a_sub_agent_named_like_a_session_is_not_that_session() {
        let dir = tempfile::tempdir().expect("tempdir");
        let store = Store::open(dir.path()).expect("open");
        let saved = |path: &str, mtime: TimestampMs, is_subagent: bool| {
            let mut r = row(path);
            r.mtime = mtime;
            r.meta = Some(SessionMeta {
                native_id: "n".into(),
                is_subagent,
                ..SessionMeta::default()
            });
            store.insert(&r).expect("insert");
            store
                .commit(&Commit::Full(Box::new(r.clone())))
                .expect("full");
            r.session
        };
        let session = saved("/t/n.jsonl", 1, false);
        saved("/t/s/subagents/n.jsonl", 2, true);
        assert_eq!(
            store
                .session_by_native(Engine::Claude, "n")
                .expect("native"),
            Some(session)
        );
    }

    #[test]
    fn rows_round_trip_and_commits_apply() {
        let dir = tempfile::tempdir().expect("tempdir");
        let store = Store::open(dir.path()).expect("open");
        let r = row("/t/a.jsonl");
        store.insert(&r).expect("insert");
        assert!(store.insert(&row("/t/a.jsonl")).is_err(), "path is unique");

        // Keys have the full u64 range.
        let keys = vec![1, u64::MAX, 1 << 63];
        store
            .commit(&Commit::Partial {
                session: r.session,
                keys: keys.clone(),
            })
            .expect("partial");
        let got = store.get(r.session).expect("get").expect("row");
        // A partial commit changes nothing but the accepted items.
        assert_eq!(got.accepted, keys.iter().copied().collect());
        assert!(!got.discovered);
        assert_eq!(got.cursor, Cursor::default());
        let found = store.find(&r.path, None).expect("find").expect("row");
        assert_eq!(found.accepted, got.accepted);

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

        // Saving the cursor empties the accepted items.
        let reopened = Store::open(dir.path()).expect("reopen");
        full.accepted.clear();
        assert_eq!(reopened.load_all().expect("load"), vec![full.clone()]);
        assert_eq!(
            reopened
                .session_by_native(Engine::Claude, "n")
                .expect("native"),
            Some(full.session)
        );
        assert_eq!(
            reopened
                .session_by_native(Engine::Codex, "n")
                .expect("native"),
            None
        );

        let found = reopened.find(&full.path, None).expect("find");
        assert_eq!(found.as_ref().map(|r| r.session), Some(full.session));
        assert!(
            reopened
                .find(&full.path, Some("x"))
                .expect("find")
                .is_none()
        );
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

    /// The start keeps of each row what the watcher needs, the same as from the whole row, and a
    /// row that cannot be read is skipped without stopping the others.
    #[test]
    fn the_index_at_start_is_what_the_rows_say() {
        let dir = tempfile::tempdir().expect("tempdir");
        let store = Store::open(dir.path()).expect("open");
        let mut session = row("/t/a.jsonl");
        session.size = 7;
        session.mtime = 9;
        session.identity = Some("1:2".into());
        session.caught_up = true;
        session.discovered = true;
        session.meta = Some(SessionMeta {
            native_id: String::new(),
            ..SessionMeta::default()
        });
        let mut sub = row("/t/s/subagents/b.jsonl");
        sub.inner_id = Some("b".into());
        sub.meta = Some(SessionMeta {
            native_id: "agent-b".into(),
            is_subagent: true,
            ..SessionMeta::default()
        });
        sub.facts.parent = Some(Parent::Session(session.session));
        let fresh = row("/t/c.jsonl");
        let broken = row("/t/d.jsonl");
        for r in [&session, &sub, &fresh, &broken] {
            store.insert(r).expect("insert");
            store
                .commit(&Commit::Full(Box::new(r.clone())))
                .expect("full");
        }
        store
            .conn
            .execute(
                "UPDATE transcripts SET facts = '{not json' WHERE session_id = ?1",
                [broken.session.0.to_string()],
            )
            .expect("break a row");

        let index = store.load_index().expect("index");
        assert_eq!(
            index,
            vec![
                Indexed::of(&session),
                Indexed::of(&fresh),
                Indexed::of(&sub)
            ]
        );
        // Without a native id in its records, a session is known by its file name.
        assert_eq!(index[0].native.as_deref(), Some("a"));
        assert_eq!(index[2].native.as_deref(), Some("agent-b"));
        assert!(index[2].subagent);
        assert_eq!(index[1].native, None, "not read yet");
    }

    fn terminal(engine: Engine, native_id: Option<&str>, cwd: &str, at: i64) -> TerminalRow {
        TerminalRow {
            terminal: TerminalId::new(),
            native_target: None,
            session: None,
            engine: Some(engine),
            native_id: native_id.map(Into::into),
            cwd: cwd.into(),
            started_at: at,
        }
    }

    #[test]
    fn started_terminals_are_claimed_by_their_sessions() {
        let dir = tempfile::tempdir().expect("tempdir");
        let store = Store::open(dir.path()).expect("open");
        let now = 10_000_000;
        let by_id = terminal(Engine::Claude, Some("abc"), "/w", now);
        let by_folder = terminal(Engine::Codex, None, "/w/p/", now - 1000);
        for t in [&by_id, &by_folder] {
            store.put_terminal(t).expect("put");
        }

        // A Claude session with another id does not take it; the one with its id does.
        let (s1, s2) = (SessionId::new(), SessionId::new());
        let claim = |s, engine, native: &str, cwd, started| {
            store
                .claim_terminal(s, engine, native, cwd, started, now)
                .expect("claim")
        };
        assert_eq!(claim(s1, Engine::Claude, "zzz", Some("/w"), now), None);
        assert_eq!(
            claim(s2, Engine::Claude, "abc", Some("/other"), now),
            Some(by_id.terminal)
        );
        // Claiming again (a replay) gives the same terminal.
        assert_eq!(
            claim(s2, Engine::Claude, "abc", Some("/other"), now),
            Some(by_id.terminal)
        );

        // Codex: by folder, only for a session that started after the terminal.
        let (old, new) = (SessionId::new(), SessionId::new());
        assert_eq!(
            claim(old, Engine::Codex, "x", Some("/w/p"), now - 60_000),
            None
        );
        assert_eq!(
            claim(new, Engine::Codex, "y", Some("/w/p"), now),
            Some(by_folder.terminal)
        );
        assert_eq!(
            store.terminal_of(new).expect("of").map(|t| t.terminal),
            Some(by_folder.terminal)
        );

        // Linking a session to a new terminal forgets its old one.
        let mut next = terminal(Engine::Claude, Some("abc"), "/w", now + 1);
        next.session = Some(s2);
        store.put_terminal(&next).expect("put");
        assert_eq!(store.terminals().expect("all").len(), 2);
        assert_eq!(
            store.terminal_of(s2).expect("of").map(|t| t.terminal),
            Some(next.terminal)
        );
        store.unlink_session(s2).expect("unlink");
        assert!(store.terminal_of(s2).expect("of").is_none());
    }

    #[test]
    fn outcomes_are_kept_for_a_week() {
        let dir = tempfile::tempdir().expect("tempdir");
        let store = Store::open(dir.path()).expect("open");
        let (old, new) = (CommandId::new(), CommandId::new());
        let ok = CommandOutcome::Ok { detail: None };
        store.save_outcome(old, &ok, 0).expect("save");
        assert_eq!(store.outcome(old).expect("get"), Some(ok.clone()));
        store
            .save_outcome(new, &ok, OUTCOME_TTL_MS + 1)
            .expect("save");
        assert_eq!(store.outcome(old).expect("get"), None);
        assert_eq!(store.outcome(new).expect("get"), Some(ok));
    }

    #[test]
    fn one_runner_per_state_directory() {
        let dir = tempfile::tempdir().expect("tempdir");
        let first = Store::open(dir.path()).expect("open");
        let second = Store::open(dir.path());
        assert!(matches!(second, Err(StoreError::Locked(_))), "{second:?}");
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
            matches!(
                err,
                StoreError::TooNew {
                    found: 99,
                    known: 2
                }
            ),
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
