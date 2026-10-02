//! Read-only access to an OpenCode SQLite store.
//!
//! The database is opened with `SQLITE_OPEN_READ_ONLY`, so SQLite never takes a write lock and
//! never creates the file. A rollback-journal store is read under a shared lock and nothing is
//! created next to it. While OpenCode has a WAL-mode store open, it is read through the existing
//! `-wal` and `-shm`; when those are gone, it is opened `immutable` (see [`quiet_wal`]),
//! since a read-only connection would create them. A reader that meets a writer's lock waits at
//! most [`BUSY_TIMEOUT`], then fails with [`io::ErrorKind::WouldBlock`] so the caller retries
//! later.
//!
//! An `immutable` read takes no lock, so a writer that starts meanwhile could change pages under
//! it. Such a read is checked: the store's length and mtime and which side files exist are noted
//! before opening and compared again by [`Store::finish`] after the last query; any change
//! discards the result as [`io::ErrorKind::WouldBlock`]. A "malformed" error during such a read
//! is the same retry only if the store changed since opening; otherwise the store itself is
//! damaged (say, a sync-conflict copy) and is reported unreadable, so it is skipped rather than
//! retried forever.
//!
//! Columns are looked up first: a store from an older or newer OpenCode version with missing
//! optional columns still reads, and one missing a required table or column is reported as
//! unreadable rather than panicking.
//!
//! The store must be a regular file, not a link to one (see `crate::open`). That is checked on a
//! handle of this crate's own before SQLite opens the store by its path, and kept true until it
//! has: on Unix, SQLite is given the path with its folders already resolved, and
//! `SQLITE_OPEN_NOFOLLOW`, so a link put in its place after the check is refused (and SQLite opens
//! every file with `O_NOFOLLOW`); on Windows, the checked handle is held without delete sharing
//! until SQLite has its own, so the file cannot be renamed, deleted or replaced in between.
//!
//! SQLite opens the side files (`-wal`, `-shm`, `-journal`) itself. Each one there is checked the
//! same way first ([`check_side_files`]), and the store is always opened with `readonly_shm=1`, so
//! SQLite never truncates or writes `-shm`, whatever it names. What is left is a race: a side file
//! swapped between the check and SQLite's own open (on Unix SQLite still refuses a link there, by
//! `O_NOFOLLOW`; a named pipe would block that read).

use crate::bound::{MAX_ID_BYTES, bounded};
use crate::lines::MAX_LINE_BYTES;
use crate::open::{FileKind, hold_transcript, open_transcript, refusal_in, refused};
use pitcrew_interfaces::source::SourceError;
use rusqlite::types::{Value, ValueRef};
use rusqlite::{Connection, ErrorCode, OpenFlags, OptionalExtension, Row, params};
use std::collections::HashSet;
use std::fs::File;
use std::io;
use std::path::{Path, PathBuf};
use std::time::{Duration, SystemTime};

/// How long a read waits for a writer's lock before giving up.
pub(crate) const BUSY_TIMEOUT: Duration = Duration::from_millis(250);

/// Ids longer than this are ignored as implausible (real ids are 30 bytes).
const MAX_ROW_ID_BYTES: usize = 256;

/// Message payloads larger than this are not parsed: their facts read as unknown.
const MAX_MESSAGE_BYTES: usize = MAX_LINE_BYTES;

/// One session row.
#[derive(Clone, Debug, Default)]
pub(crate) struct SessionRow {
    pub id: String,
    pub parent_id: Option<String>,
    pub directory: Option<String>,
    pub title: Option<String>,
    pub model: Option<String>,
    pub created: i64,
    pub updated: i64,
}

/// One part row, without its payload.
#[derive(Clone, Debug)]
pub(crate) struct PartRow {
    pub id: String,
    pub message_id: String,
    pub created: i64,
    pub updated: i64,
    /// Payload size in bytes; `None` if there is no payload.
    pub size: Option<u64>,
}

/// One message row, without its payload.
#[derive(Clone, Debug)]
pub(crate) struct MessageRow {
    pub id: String,
    pub created: i64,
}

/// Where a walk over a session's messages continues: below this `(time_created, id)`, compared
/// as SQLite stores them.
#[derive(Clone, Debug)]
pub(crate) struct MessageKey(Value, String);

/// One message of a walk, newest first.
#[derive(Clone, Debug)]
pub(crate) struct MessageStep {
    pub id: String,
    pub created: i64,
    /// Whether the message has no parts.
    pub partless: bool,
    pub key: MessageKey,
}

/// What a message's payload says, read by SQLite's JSON functions so large payloads are never
/// copied out.
#[derive(Clone, Debug, Default)]
pub(crate) struct MessageFacts {
    pub role: Option<String>,
    pub completed: bool,
    pub failed: bool,
    /// `time.completed`, when it is a number.
    pub ended: Option<i64>,
    pub model: Option<String>,
}

/// The columns of the tables the adapter reads.
#[derive(Debug, Default)]
struct Schema {
    session: HashSet<String>,
    message: HashSet<String>,
    part: HashSet<String>,
}

/// What an unlocked read compares before and after: the store's length and mtime, and which side
/// files exist.
#[derive(Clone, Debug, PartialEq, Eq)]
struct FileState {
    len: u64,
    modified: Option<SystemTime>,
    wal: bool,
    shm: bool,
}

impl FileState {
    fn of(path: &Path) -> Option<Self> {
        let meta = std::fs::metadata(path).ok()?;
        Some(Self {
            len: meta.len(),
            modified: meta.modified().ok(),
            wal: side_file(path, "-wal").exists(),
            shm: side_file(path, "-shm").exists(),
        })
    }
}

/// An open, read-only store, inside one read transaction so every query sees the same snapshot.
pub(crate) struct Store {
    conn: Connection,
    path: PathBuf,
    schema: Schema,
    part_rowid: bool,
    /// For an `immutable` (unlocked) read, the store as it was before opening.
    unlocked: Option<FileState>,
}

impl Store {
    /// Opens `path` read-only, if it and its side files are regular files (see the module docs).
    pub(crate) fn open(path: &Path) -> Result<Self, SourceError> {
        let held = hold_store(path)?;
        check_side_files(path)?;
        let target = sqlite_path(path)?;
        let quiet = quiet_wal(path, &held);
        let unlocked = quiet.clone();
        let err = |e| sql_error(path, e, unlocked.as_ref());
        let flags = OpenFlags::SQLITE_OPEN_READ_ONLY
            | OpenFlags::SQLITE_OPEN_NO_MUTEX
            | OpenFlags::SQLITE_OPEN_URI
            | NOFOLLOW;
        // Always a URI, so `readonly_shm` applies: SQLite then opens `-shm` read-only and never
        // truncates or writes it, whatever that path names.
        let params = if quiet.is_some() {
            "mode=ro&immutable=1&readonly_shm=1"
        } else {
            "mode=ro&readonly_shm=1"
        };
        let uri = file_uri(&target, params).ok_or_else(|| SourceError::Unreadable {
            path: path.to_path_buf(),
            reason: "the store's path is not Unicode".into(),
        })?;
        let conn = Connection::open_with_flags(&uri, flags)
            .map_err(|e| open_error(path, e, unlocked.as_ref()))?;
        // SQLite has its own handle on the store now.
        drop(held);
        conn.busy_timeout(BUSY_TIMEOUT).map_err(err)?;
        conn.pragma_update(None, "query_only", true).map_err(err)?;
        conn.execute_batch("BEGIN").map_err(err)?;
        let schema = Schema {
            session: columns(&conn, "session").map_err(err)?,
            message: columns(&conn, "message").map_err(err)?,
            part: columns(&conn, "part").map_err(err)?,
        };
        let part_rowid = conn.prepare("SELECT rowid FROM part LIMIT 0").is_ok();
        Ok(Self {
            conn,
            path: path.to_path_buf(),
            schema,
            part_rowid,
            unlocked,
        })
    }

    /// Call after the last query: an unlocked read of a store that changed meanwhile may have
    /// mixed old and new pages, so it fails with [`io::ErrorKind::WouldBlock`] to be retried.
    pub(crate) fn finish(&self) -> Result<(), SourceError> {
        match &self.unlocked {
            Some(before) if FileState::of(&self.path).as_ref() != Some(before) => {
                Err(retry_later(&self.path, "changed during an unlocked read"))
            }
            _ => Ok(()),
        }
    }

    /// One row from a cached statement: reads run the same few queries per part.
    fn cached_row<T, P: rusqlite::Params>(
        &self,
        sql: &str,
        params: P,
        f: impl FnOnce(&Row<'_>) -> rusqlite::Result<T>,
    ) -> rusqlite::Result<T> {
        self.conn.prepare_cached(sql)?.query_row(params, f)
    }

    fn err(&self, e: rusqlite::Error) -> SourceError {
        sql_error(&self.path, e, self.unlocked.as_ref())
    }

    fn unreadable(&self, reason: String) -> SourceError {
        SourceError::Unreadable {
            path: self.path.clone(),
            reason,
        }
    }

    /// Whether the store has sessions to list.
    pub(crate) fn has_sessions(&self) -> bool {
        self.schema.session.contains("id")
    }

    /// Checks the tables and columns a transcript read needs.
    pub(crate) fn require_transcripts(&self) -> Result<(), SourceError> {
        let need: [(&str, &HashSet<String>, &[&str]); 3] = [
            ("session", &self.schema.session, &["id"]),
            (
                "message",
                &self.schema.message,
                &["id", "session_id", "data"],
            ),
            (
                "part",
                &self.schema.part,
                &["id", "message_id", "session_id", "data"],
            ),
        ];
        for (table, cols, required) in need {
            if cols.is_empty() {
                return Err(self.unreadable(format!("no `{table}` table in the OpenCode store")));
            }
            if let Some(missing) = required.iter().find(|c| !cols.contains(**c)) {
                return Err(self.unreadable(format!("`{table}` has no `{missing}` column")));
            }
        }
        Ok(())
    }

    /// The query behind [`Store::sessions`]. Its work is one index lookup per session: payloads
    /// are never read.
    fn sessions_sql(&self) -> String {
        format!(
            "SELECT s.id, {parent}, {updated}, {change} FROM session s",
            parent = col(&self.schema.session, "s.parent_id", "parent_id", "NULL"),
            updated = col(&self.schema.session, "s.time_updated", "time_updated", "0"),
            change = if self.part_rowid && self.schema.part.contains("session_id") {
                // `part_session_idx` answers this from the end of the session's range.
                "(SELECT max(p.rowid) FROM part p WHERE p.session_id = s.id)"
            } else {
                "0"
            },
        )
    }

    /// Every session, with the highest rowid of its parts (0 without parts).
    pub(crate) fn sessions(&self) -> Result<Vec<(SessionRow, u64)>, SourceError> {
        let mut stmt = self
            .conn
            .prepare(&self.sessions_sql())
            .map_err(|e| self.err(e))?;
        let rows = stmt
            .query_map([], |row| {
                Ok((
                    text(row, 0),
                    SessionRow {
                        parent_id: text(row, 1),
                        updated: int(row, 2),
                        ..SessionRow::default()
                    },
                    u64::try_from(int(row, 3)).unwrap_or(0),
                ))
            })
            .map_err(|e| self.err(e))?;
        let mut out = Vec::new();
        for row in rows {
            let (id, mut session, change) = row.map_err(|e| self.err(e))?;
            let Some(id) = id.filter(|id| id.len() <= MAX_ROW_ID_BYTES) else {
                continue;
            };
            session.id = id;
            out.push((session, change));
        }
        Ok(out)
    }

    /// One session row.
    pub(crate) fn session(&self, id: &str) -> Result<Option<SessionRow>, SourceError> {
        let s = &self.schema.session;
        let sql = format!(
            "SELECT {}, {}, {}, {}, {}, {} FROM session WHERE id = ?1",
            col(s, "parent_id", "parent_id", "NULL"),
            col(s, "directory", "directory", "NULL"),
            col(s, "title", "title", "NULL"),
            col(s, "model", "model", "NULL"),
            col(s, "time_created", "time_created", "0"),
            col(s, "time_updated", "time_updated", "0"),
        );
        self.cached_row(&sql, params![id], |row| {
            Ok(SessionRow {
                id: id.to_owned(),
                parent_id: text(row, 0),
                directory: text(row, 1),
                title: text(row, 2),
                model: text(row, 3),
                created: int(row, 4),
                updated: int(row, 5),
            })
        })
        .optional()
        .map_err(|e| self.err(e))
    }

    /// The session's parts, without payloads, in no particular order.
    pub(crate) fn parts(&self, session: &str) -> Result<Vec<PartRow>, SourceError> {
        let p = &self.schema.part;
        let sql = format!(
            "SELECT id, message_id, {}, {}, octet_length(data) FROM part WHERE session_id = ?1",
            col(p, "time_created", "time_created", "0"),
            col(p, "time_updated", "time_updated", "0"),
        );
        let mut stmt = self.conn.prepare(&sql).map_err(|e| self.err(e))?;
        let rows = stmt
            .query_map(params![session], |row| {
                Ok((
                    text(row, 0),
                    text(row, 1),
                    int(row, 2),
                    int(row, 3),
                    opt_int(row, 4),
                ))
            })
            .map_err(|e| self.err(e))?;
        let mut out = Vec::new();
        for row in rows {
            let (id, message_id, created, updated, size) = row.map_err(|e| self.err(e))?;
            if let Some(row) = part_row(id, message_id, created, updated, size) {
                out.push(row);
            }
        }
        Ok(out)
    }

    /// Whether parts can be walked by rowid: the `part_session_idx` index orders a session's
    /// parts by rowid, which is insertion order. A `WITHOUT ROWID` table cannot.
    pub(crate) fn parts_have_rowid(&self) -> bool {
        self.part_rowid
    }

    /// Up to `limit` of the session's parts with a rowid below `below` (all when `None`),
    /// newest rowid first, each with its rowid.
    pub(crate) fn parts_below(
        &self,
        session: &str,
        below: Option<i64>,
        limit: usize,
    ) -> Result<Vec<(i64, Option<PartRow>)>, SourceError> {
        // An inclusive bound, so a row at `i64::MAX` is read too.
        let upper = match below {
            None => i64::MAX,
            Some(b) => match b.checked_sub(1) {
                Some(upper) => upper,
                None => return Ok(Vec::new()),
            },
        };
        let p = &self.schema.part;
        let sql = format!(
            "SELECT rowid, id, message_id, {}, {}, octet_length(data) FROM part
             WHERE session_id = ?1 AND rowid <= ?2 ORDER BY rowid DESC LIMIT ?3",
            col(p, "time_created", "time_created", "0"),
            col(p, "time_updated", "time_updated", "0"),
        );
        let mut stmt = self.conn.prepare_cached(&sql).map_err(|e| self.err(e))?;
        let limit = i64::try_from(limit).unwrap_or(i64::MAX);
        let rows = stmt
            .query_map(params![session, upper, limit], |row| {
                Ok((
                    int(row, 0),
                    text(row, 1),
                    text(row, 2),
                    int(row, 3),
                    int(row, 4),
                    opt_int(row, 5),
                ))
            })
            .map_err(|e| self.err(e))?;
        let mut out = Vec::new();
        for row in rows {
            let (rowid, id, message_id, created, updated, size) = row.map_err(|e| self.err(e))?;
            // Rows with unusable ids are `None`: passed over, but they still move the walk on.
            out.push((rowid, part_row(id, message_id, created, updated, size)));
        }
        Ok(out)
    }

    /// The lowest and highest rowid of the session's parts.
    pub(crate) fn part_rowid_bounds(
        &self,
        session: &str,
    ) -> Result<Option<(i64, i64)>, SourceError> {
        // Two queries: SQLite answers a lone min() or max() from the index, but not both at once.
        let bound = |sql: &str| {
            self.cached_row(sql, params![session], |row| Ok(opt_int(row, 0)))
                .map_err(|e| self.err(e))
        };
        let min = bound("SELECT min(rowid) FROM part WHERE session_id = ?1")?;
        let max = bound("SELECT max(rowid) FROM part WHERE session_id = ?1")?;
        Ok(min.zip(max))
    }

    /// The session's part with the highest rowid at or below `rowid`: its rowid, id and
    /// creation time.
    pub(crate) fn part_at_or_below(
        &self,
        session: &str,
        rowid: i64,
    ) -> Result<Option<(i64, String, i64)>, SourceError> {
        let sql = format!(
            "SELECT rowid, id, {} FROM part WHERE session_id = ?1 AND rowid <= ?2
             ORDER BY rowid DESC LIMIT 1",
            col(&self.schema.part, "time_created", "time_created", "0"),
        );
        self.cached_row(&sql, params![session, rowid], |row| {
            Ok((int(row, 0), text(row, 1).unwrap_or_default(), int(row, 2)))
        })
        .optional()
        .map_err(|e| self.err(e))
    }

    /// The ids and creation times of one message's parts.
    pub(crate) fn message_parts(&self, message: &str) -> Result<Vec<(String, i64)>, SourceError> {
        let sql = format!(
            "SELECT id, {} FROM part WHERE message_id = ?1",
            col(&self.schema.part, "time_created", "time_created", "0"),
        );
        let mut stmt = self.conn.prepare_cached(&sql).map_err(|e| self.err(e))?;
        let rows = stmt
            .query_map(params![message], |row| Ok((text(row, 0), int(row, 1))))
            .map_err(|e| self.err(e))?;
        let mut out = Vec::new();
        for row in rows {
            let (id, created) = row.map_err(|e| self.err(e))?;
            if let Some(id) = id.filter(|id| id.len() <= MAX_ROW_ID_BYTES) {
                out.push((id, created));
            }
        }
        Ok(out)
    }

    /// The session's newest assistant message.
    pub(crate) fn newest_assistant(
        &self,
        session: &str,
    ) -> Result<Option<MessageRow>, SourceError> {
        let m = &self.schema.message;
        let sql = format!(
            "SELECT id, {created} FROM message WHERE session_id = ?1
               AND CASE WHEN typeof(data) != 'text' THEN 0
                        WHEN octet_length(data) > {MAX_MESSAGE_BYTES} THEN 0
                        WHEN json_valid(data) THEN json_extract(data, '$.role') = 'assistant'
                        ELSE 0 END
             ORDER BY {order} DESC, id DESC LIMIT 1",
            created = col(m, "time_created", "time_created", "0"),
            // A literal would be read as a column number.
            order = col(m, "time_created", "time_created", "rowid"),
        );
        self.cached_row(&sql, params![session], |row| {
            Ok(text(row, 0)
                .filter(|id| id.len() <= MAX_ROW_ID_BYTES)
                .map(|id| MessageRow {
                    id,
                    created: int(row, 1),
                }))
        })
        .optional()
        .map(Option::flatten)
        .map_err(|e| self.err(e))
    }

    /// Up to `limit` of the session's messages below `below` (from the newest when `None`),
    /// newest first by `(time_created, id)`.
    pub(crate) fn messages_below(
        &self,
        session: &str,
        below: Option<&MessageKey>,
        limit: usize,
    ) -> Result<Vec<MessageStep>, SourceError> {
        let timed = self.schema.message.contains("time_created");
        let (created, order) = if timed {
            ("m.time_created", "m.time_created DESC, m.id DESC")
        } else {
            ("0", "m.id DESC")
        };
        let bound = match (below, timed) {
            (None, _) => "",
            (Some(_), true) => "AND (m.time_created, m.id) < (?2, ?3)",
            (Some(_), false) => "AND m.id < ?3",
        };
        let partless = if self.schema.part.contains("message_id") {
            "NOT EXISTS (SELECT 1 FROM part p WHERE p.message_id = m.id)"
        } else {
            "0"
        };
        let sql = format!(
            "SELECT {created}, m.id, {partless} FROM message m
             WHERE m.session_id = ?1 AND typeof(m.id) = 'text'
               AND octet_length(m.id) <= {MAX_ROW_ID_BYTES} {bound}
             ORDER BY {order} LIMIT ?4"
        );
        let (top, id) = match below {
            Some(MessageKey(created, id)) => (created.clone(), id.clone()),
            None => (Value::Null, String::new()),
        };
        let limit = i64::try_from(limit).unwrap_or(i64::MAX);
        let mut stmt = self.conn.prepare_cached(&sql).map_err(|e| self.err(e))?;
        let rows = stmt
            .query_map(params![session, top, id, limit], |row| {
                Ok((
                    row.get::<_, Value>(0)?,
                    int(row, 0),
                    text(row, 1),
                    int(row, 2),
                ))
            })
            .map_err(|e| self.err(e))?;
        let mut out = Vec::new();
        for row in rows {
            let (raw, created, id, partless) = row.map_err(|e| self.err(e))?;
            if let Some(id) = id {
                out.push(MessageStep {
                    key: MessageKey(raw, id.clone()),
                    id,
                    created,
                    partless: partless != 0,
                });
            }
        }
        Ok(out)
    }

    /// Where a walk for messages below position-time `ms` can start: below the first message
    /// created at or after `ms`, as long as that message's id time `check` confirms it is not
    /// older (`check` gets its id and creation time). `None` starts at the top.
    pub(crate) fn messages_from(
        &self,
        session: &str,
        ms: i64,
        check: impl FnOnce(&str, i64) -> bool,
    ) -> Result<Option<MessageKey>, SourceError> {
        if !self.schema.message.contains("time_created") {
            return Ok(None);
        }
        let first = self
            .cached_row(
                "SELECT id, time_created FROM message
                 WHERE session_id = ?1 AND time_created >= ?2 AND typeof(id) = 'text'
                 ORDER BY time_created, id LIMIT 1",
                params![session, ms],
                |row| Ok((text(row, 0), int(row, 1))),
            )
            .optional()
            .map_err(|e| self.err(e))?;
        Ok(match first {
            // Nothing that new: every message is below.
            None => Some(MessageKey(Value::Integer(ms), String::new())),
            Some((Some(id), created)) if check(&id, created) => {
                Some(MessageKey(Value::Integer(ms), String::new()))
            }
            _ => None,
        })
    }

    /// A message's creation time, if the message exists.
    pub(crate) fn message_created(&self, id: &str) -> Result<Option<i64>, SourceError> {
        let sql = format!(
            "SELECT {} FROM message WHERE id = ?1",
            col(&self.schema.message, "time_created", "time_created", "0"),
        );
        self.cached_row(&sql, params![id], |row| Ok(int(row, 0)))
            .optional()
            .map_err(|e| self.err(e))
    }

    /// One part's payload as bytes (it may not be valid UTF-8).
    pub(crate) fn part_data(&self, id: &str) -> Result<Option<Vec<u8>>, SourceError> {
        self.cached_row(
            "SELECT CAST(data AS BLOB) FROM part WHERE id = ?1",
            params![id],
            |row| {
                Ok(row
                    .get_ref(0)?
                    .as_blob_or_null()
                    .ok()
                    .flatten()
                    .map(<[u8]>::to_vec))
            },
        )
        .optional()
        .map(Option::flatten)
        .map_err(|e| self.err(e))
    }

    /// What one message's payload says. Payloads that are not text, larger than
    /// [`MAX_MESSAGE_BYTES`] or not JSON give empty facts.
    pub(crate) fn message_facts(&self, id: &str) -> Result<MessageFacts, SourceError> {
        // The size is checked before the payload is parsed.
        let sql = format!(
            "SELECT
            CASE WHEN json_type(d, '$.role') = 'text' THEN json_extract(d, '$.role') END,
            json_type(d, '$.time.completed') IS NOT NULL AND json_type(d, '$.time.completed') != 'null',
            json_type(d, '$.error') IS NOT NULL AND json_type(d, '$.error') != 'null',
            CASE WHEN json_type(d, '$.time.completed') IN ('integer', 'real')
                 THEN json_extract(d, '$.time.completed') END,
            CASE WHEN json_type(d, '$.modelID') = 'text'
                  AND octet_length(json_extract(d, '$.modelID')) <= {MAX_ID_BYTES}
                 THEN json_extract(d, '$.modelID') END
            FROM (SELECT CASE WHEN typeof(data) != 'text' THEN NULL
                              WHEN octet_length(data) > {MAX_MESSAGE_BYTES} THEN NULL
                              WHEN json_valid(data) THEN data END AS d
                  FROM message WHERE id = ?1)"
        );
        self.cached_row(&sql, params![id], |row| {
            Ok(MessageFacts {
                role: text(row, 0),
                completed: int(row, 1) != 0,
                failed: int(row, 2) != 0,
                ended: opt_int(row, 3),
                model: bounded(text(row, 4).as_deref(), MAX_ID_BYTES),
            })
        })
        .optional()
        .map(Option::unwrap_or_default)
        .map_err(|e| self.err(e))
    }

    /// The model of the newest assistant message that names one.
    pub(crate) fn latest_model(&self, session: &str) -> Result<Option<String>, SourceError> {
        let sql = format!(
            "SELECT id FROM message WHERE session_id = ?1 ORDER BY {} DESC, id DESC LIMIT 20",
            // A literal would be read as a column number.
            col(
                &self.schema.message,
                "time_created",
                "time_created",
                "rowid"
            ),
        );
        let mut stmt = self.conn.prepare(&sql).map_err(|e| self.err(e))?;
        let ids: Vec<String> = stmt
            .query_map(params![session], |row| Ok(text(row, 0)))
            .map_err(|e| self.err(e))?
            .filter_map(|r| r.ok().flatten())
            .collect();
        for id in ids {
            let facts = self.message_facts(&id)?;
            if facts.role.as_deref() == Some("assistant") && facts.model.is_some() {
                return Ok(facts.model);
            }
        }
        Ok(None)
    }
}

fn part_row(
    id: Option<String>,
    message_id: Option<String>,
    created: i64,
    updated: i64,
    size: Option<i64>,
) -> Option<PartRow> {
    match (id, message_id) {
        (Some(id), Some(message_id))
            if id.len() <= MAX_ROW_ID_BYTES && message_id.len() <= MAX_ROW_ID_BYTES =>
        {
            Some(PartRow {
                id,
                message_id,
                created,
                updated,
                size: size.map(|n| u64::try_from(n).unwrap_or(0)),
            })
        }
        _ => None,
    }
}

/// SQLite refuses a path with a link in it (Unix only: SQLite's Windows build ignores the flag).
#[cfg(unix)]
const NOFOLLOW: OpenFlags = OpenFlags::SQLITE_OPEN_NOFOLLOW;
#[cfg(not(unix))]
const NOFOLLOW: OpenFlags = OpenFlags::empty();

/// The store, opened and checked (see `crate::open`). A link, a named pipe or anything else that
/// is not a regular file is refused; a store that cannot be opened at all is unreadable, as when
/// SQLite reported it.
fn hold_store(path: &Path) -> Result<File, SourceError> {
    hold_transcript(path).map_err(|e| {
        if refusal_in(&e).is_some() {
            SourceError::Io(e)
        } else {
            SourceError::Unreadable {
                path: path.to_path_buf(),
                reason: format!("cannot open the store: {e}"),
            }
        }
    })
}

/// The path SQLite opens. On Unix, the store's folder resolved, so the only link
/// `SQLITE_OPEN_NOFOLLOW` could meet is one put in place of the store itself (folders above the
/// store may be links, as discovery allows). Elsewhere the path as it is.
#[cfg(unix)]
fn sqlite_path(path: &Path) -> Result<PathBuf, SourceError> {
    let unreadable = |reason: String| SourceError::Unreadable {
        path: path.to_path_buf(),
        reason,
    };
    let name = path
        .file_name()
        .ok_or_else(|| unreadable("the store's path has no file name".into()))?;
    let folder = match path.parent() {
        Some(p) if !p.as_os_str().is_empty() => p,
        _ => Path::new("."),
    };
    let folder = std::fs::canonicalize(folder)
        .map_err(|e| unreadable(format!("cannot resolve the store's folder: {e}")))?;
    Ok(folder.join(name))
}

#[cfg(not(unix))]
fn sqlite_path(path: &Path) -> Result<PathBuf, SourceError> {
    Ok(path.to_path_buf())
}

/// [`sql_error`], except that SQLite meeting a link (`SQLITE_CANTOPEN_SYMLINK`) is a refusal.
fn open_error(path: &Path, e: rusqlite::Error, unlocked: Option<&FileState>) -> SourceError {
    if let rusqlite::Error::SqliteFailure(f, _) = &e
        && f.extended_code == rusqlite::ffi::SQLITE_CANTOPEN_SYMLINK
    {
        return SourceError::Io(refused(path, FileKind::Link));
    }
    sql_error(path, e, unlocked)
}

fn side_file(path: &Path, suffix: &str) -> PathBuf {
    let mut name = path.as_os_str().to_owned();
    name.push(suffix);
    PathBuf::from(name)
}

/// SQLite opens a store's side files itself, by their paths. Each one there (`-wal`, `-shm`,
/// `-journal`) must be a regular file of its own (see `crate::open`: on Unix one hard link), or
/// the store is refused before SQLite opens anything: a named pipe there would block the read,
/// and with it the runner's whole watcher thread, and a link at `-shm` names a file a read-write
/// open would truncate and overwrite (`readonly_shm` keeps this crate's own open from doing so).
/// A side file that is not there is fine.
fn check_side_files(path: &Path) -> Result<(), SourceError> {
    for suffix in ["-wal", "-shm", "-journal"] {
        let side = side_file(path, suffix);
        match open_transcript(&side) {
            Ok(_) => {}
            Err(e) if e.kind() == io::ErrorKind::NotFound => {}
            Err(e) if refusal_in(&e).is_some() => return Err(SourceError::Io(e)),
            Err(e) => {
                return Err(SourceError::Unreadable {
                    path: path.to_path_buf(),
                    reason: format!("cannot open the store's {suffix} file: {e}"),
                });
            }
        }
    }
    Ok(())
}

/// For a WAL-mode store whose `-wal` or `-shm` file is missing (OpenCode is not running), the
/// store's state before opening: it is then opened `immutable`, since a read-only connection
/// would otherwise create those files. With both present, `None`, and the store is read through
/// them like any WAL reader.
///
/// `immutable` skips locking, which is safe while nothing writes. If OpenCode starts during the
/// read, its writes go to a new `-wal`, and only a checkpoint rewrites the main file; either is
/// seen as a change of the store's state, by [`Store::finish`] or when a "malformed" error is
/// met, and the read is retried. A `-wal` left
/// without its `-shm` by a crash is not read until OpenCode recovers it.
///
/// `held` is the store, already checked and open.
fn quiet_wal(path: &Path, held: &File) -> Option<FileState> {
    use std::io::Read;
    // Noted first, so any change from here on is seen. When it cannot be, SQLite reports the
    // missing or unreadable file.
    let before = FileState::of(path)?;
    let mut header = [0u8; 20];
    let mut file = held;
    let n = file.read(&mut header).ok()?;
    let wal_mode = n == header.len() && (header[18] == 2 || header[19] == 2);
    (wal_mode && !(before.wal && before.shm)).then_some(before)
}

/// `target` as a `file:` URI with `params`, every byte but unreserved ones percent-encoded. `None`
/// for a path that is not Unicode on Windows (a URI holds UTF-8 there).
fn file_uri(target: &Path, params: &str) -> Option<String> {
    #[cfg(unix)]
    let bytes = {
        use std::os::unix::ffi::OsStrExt as _;
        target.as_os_str().as_bytes().to_vec()
    };
    #[cfg(not(unix))]
    let bytes = target.to_str()?.replace('\\', "/").into_bytes();
    let mut uri = String::from("file://");
    if bytes.first() != Some(&b'/') {
        uri.push('/');
    }
    for b in bytes {
        if b.is_ascii_alphanumeric() || b"/-._~:".contains(&b) {
            uri.push(char::from(b));
        } else {
            uri.push_str(&format!("%{b:02X}"));
        }
    }
    uri.push('?');
    uri.push_str(params);
    Some(uri)
}

/// `expr` if `table` has `column`, else `fallback`.
fn col<'a>(table: &HashSet<String>, expr: &'a str, column: &str, fallback: &'a str) -> &'a str {
    if table.contains(column) {
        expr
    } else {
        fallback
    }
}

fn columns(conn: &Connection, table: &str) -> rusqlite::Result<HashSet<String>> {
    let mut stmt = conn.prepare("SELECT name FROM pragma_table_info(?1)")?;
    let names = stmt.query_map(params![table], |row| row.get::<_, String>(0))?;
    names.collect()
}

/// A text value, whatever the column's declared type; `None` for anything else.
fn text(row: &Row<'_>, i: usize) -> Option<String> {
    match row.get_ref(i).ok()? {
        ValueRef::Text(t) => std::str::from_utf8(t).ok().map(str::to_owned),
        _ => None,
    }
}

fn opt_int(row: &Row<'_>, i: usize) -> Option<i64> {
    match row.get_ref(i).ok()? {
        ValueRef::Integer(n) => Some(n),
        // Real columns hold whole milliseconds in practice; anything else reads as missing.
        #[allow(clippy::cast_possible_truncation)]
        ValueRef::Real(f) if f.is_finite() => Some(f as i64),
        _ => None,
    }
}

fn int(row: &Row<'_>, i: usize) -> i64 {
    opt_int(row, i).unwrap_or(0)
}

fn retry_later(path: &Path, why: &str) -> SourceError {
    SourceError::Io(io::Error::new(
        io::ErrorKind::WouldBlock,
        format!("{}: {why}; retry later", path.display()),
    ))
}

/// A busy or locked store is a retry-later I/O error. So is a "malformed" page met by an
/// unlocked read (`unlocked` holds the store's state before opening) of a store that has changed
/// since: a writer may have been checkpointing. A store that has not changed is damaged, and
/// that, like anything else, makes it unreadable.
fn sql_error(path: &Path, e: rusqlite::Error, unlocked: Option<&FileState>) -> SourceError {
    match e.sqlite_error_code() {
        Some(ErrorCode::DatabaseBusy | ErrorCode::DatabaseLocked) => {
            retry_later(path, "locked by a writer")
        }
        Some(ErrorCode::DatabaseCorrupt)
            if unlocked.is_some_and(|before| FileState::of(path).as_ref() != Some(before)) =>
        {
            retry_later(path, "changed during an unlocked read")
        }
        _ => SourceError::Unreadable {
            path: path.to_path_buf(),
            reason: e.to_string(),
        },
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use rusqlite::StatementStatus;

    fn wal_store(dir: &Path) -> PathBuf {
        let path = dir.join("opencode.db");
        let conn = Connection::open(&path).expect("open");
        let mode: String = conn
            .query_row("PRAGMA journal_mode = WAL", [], |r| r.get(0))
            .expect("wal");
        assert_eq!(mode, "wal");
        conn.execute_batch(
            "CREATE TABLE session (id TEXT PRIMARY KEY, time_updated INTEGER);
             CREATE TABLE message (id TEXT PRIMARY KEY, session_id TEXT, time_created INTEGER, data TEXT);
             CREATE TABLE part (id TEXT PRIMARY KEY, message_id TEXT, session_id TEXT, data TEXT);
             CREATE INDEX part_session_idx ON part (session_id);
             INSERT INTO session VALUES ('ses_a', 5);",
        )
        .expect("schema");
        drop(conn);
        assert!(!side_file(&path, "-wal").exists(), "closed cleanly");
        path
    }

    /// `Store::open` on another thread, failing the test if it does not return within 10 s (a
    /// named pipe opened for reading blocks until someone writes it).
    #[cfg(unix)]
    fn open_within(path: &Path) -> Result<Store, SourceError> {
        let (tx, rx) = std::sync::mpsc::channel();
        let p = path.to_path_buf();
        std::thread::spawn(move || {
            let _ = tx.send(Store::open(&p));
        });
        rx.recv_timeout(Duration::from_secs(10))
            .unwrap_or_else(|_| panic!("opening {} blocked", path.display()))
    }

    #[cfg(unix)]
    fn mkfifo(path: &Path) {
        let made = std::process::Command::new("mkfifo")
            .arg(path)
            .status()
            .expect("mkfifo");
        assert!(made.success(), "mkfifo failed");
    }

    /// A named pipe in place of a rollback-journal store's `-journal`, which SQLite opens
    /// read-only to look for a hot journal (an open that waits for a writer on a pipe), is refused
    /// before SQLite opens anything, so the read cannot block on it.
    #[cfg(unix)]
    #[test]
    fn a_named_pipe_for_a_journal_is_refused_without_blocking() {
        let dir = tempfile::tempdir().expect("tempdir");
        let journal = dir.path().join("journal-store.db");
        let conn = Connection::open(&journal).expect("open");
        conn.execute_batch("CREATE TABLE session (id TEXT PRIMARY KEY);")
            .expect("schema");
        drop(conn);
        mkfifo(&side_file(&journal, "-journal"));
        let Err(err) = open_within(&journal) else {
            panic!("opened");
        };
        assert_eq!(
            crate::open::refusal(&err).map(|r| (r.kind, r.path.clone())),
            Some((FileKind::Fifo, side_file(&journal, "-journal")))
        );
    }

    /// A named pipe in place of `-wal` (with `-shm` there, so the store is read through them) is
    /// refused too. SQLite opens `-wal` read-write, which a pipe does not block on Linux, but it is
    /// no WAL, and other systems may block.
    #[cfg(unix)]
    #[test]
    fn a_named_pipe_for_a_wal_is_refused() {
        let dir = tempfile::tempdir().expect("tempdir");
        let path = wal_store(dir.path());
        mkfifo(&side_file(&path, "-wal"));
        std::fs::write(side_file(&path, "-shm"), b"").expect("shm");
        let Err(err) = open_within(&path) else {
            panic!("opened");
        };
        assert_eq!(
            crate::open::refusal(&err).map(|r| (r.kind, r.path.clone())),
            Some((FileKind::Fifo, side_file(&path, "-wal")))
        );
    }

    /// A store a writer left with frames in its `-wal` (a crash, or OpenCode still running in
    /// another process), whose `-shm` is hard-linked to another file, is refused before SQLite
    /// opens anything, and the other file is left exactly as it was: a read-write open of `-shm`
    /// would truncate and overwrite it.
    #[cfg(unix)]
    #[test]
    fn a_hard_linked_shm_is_refused_and_its_target_is_untouched() {
        let dir = tempfile::tempdir().expect("tempdir");
        let path = wal_store(dir.path());
        let writer = Connection::open(&path).expect("writer");
        writer
            .set_db_config(
                rusqlite::config::DbConfig::SQLITE_DBCONFIG_NO_CKPT_ON_CLOSE,
                true,
            )
            .expect("no checkpoint on close");
        writer
            .execute("INSERT INTO session VALUES ('ses_b', 6)", [])
            .expect("write");
        // Closed without a checkpoint: `-wal` keeps its frames and `-shm` stays, as after a crash.
        // (A writer open in this process would share its own `-shm` with the reader instead.)
        drop(writer);
        assert!(
            std::fs::metadata(side_file(&path, "-wal"))
                .expect("wal")
                .len()
                > 0
        );
        assert!(side_file(&path, "-shm").exists());
        let victim = dir.path().join("victim");
        let bytes: Vec<u8> = (0..64 * 1024).map(|i| (i % 251) as u8).collect();
        std::fs::write(&victim, &bytes).expect("victim");
        let shm = std::fs::read(side_file(&path, "-shm")).expect("shm");
        std::fs::remove_file(side_file(&path, "-shm")).expect("rm shm");
        std::fs::hard_link(&victim, side_file(&path, "-shm")).expect("hard link");

        let opened = open_within(&path).map(|store| store.sessions().map(|s| s.len()));
        assert!(
            std::fs::read(&victim).expect("victim") == bytes,
            "the hard link's target was written"
        );
        let err = opened.expect_err("refused");
        assert_eq!(
            crate::open::refusal(&err).map(|r| (r.kind, r.path.clone())),
            Some((FileKind::HardLinked, side_file(&path, "-shm")))
        );

        // With its own `-shm` back, the store the writer left still reads, through
        // `readonly_shm`, with the row only its `-wal` holds.
        std::fs::remove_file(side_file(&path, "-shm")).expect("rm link");
        std::fs::write(side_file(&path, "-shm"), &shm).expect("shm");
        let store = open_within(&path).expect("open");
        assert_eq!(store.sessions().expect("sessions").len(), 2);
    }

    /// What happens if the store becomes a link after this crate's own check and before SQLite
    /// opens it: SQLite refuses it (its folders are resolved, so only the store itself could be
    /// the link), and that is a refusal like the check's own. A linked folder above it is fine.
    #[cfg(unix)]
    #[test]
    fn sqlite_refuses_a_store_that_became_a_link_but_not_a_linked_folder() {
        use std::os::unix::fs::symlink;
        let dir = tempfile::tempdir().expect("tempdir");
        let real = wal_store(dir.path());
        let flags = OpenFlags::SQLITE_OPEN_READ_ONLY | OpenFlags::SQLITE_OPEN_NO_MUTEX | NOFOLLOW;

        let link = dir.path().join("opencode-link.db");
        symlink(&real, &link).expect("link");
        let target = sqlite_path(&link).expect("resolve");
        let e = Connection::open_with_flags(&target, flags).expect_err("a link");
        let err = open_error(&link, e, None);
        let refused = crate::open::refusal(&err).expect("a refusal");
        assert_eq!(
            (refused.kind, refused.path.as_path()),
            (FileKind::Link, link.as_path())
        );

        let folder = dir.path().join("linked-home");
        symlink(dir.path(), &folder).expect("link");
        let target = sqlite_path(&folder.join("opencode.db")).expect("resolve");
        assert!(!target.starts_with(&folder), "{}", target.display());
        let conn = Connection::open_with_flags(&target, flags).expect("a linked folder opens");
        drop(conn);
        let store = Store::open(&folder.join("opencode.db")).expect("open");
        assert_eq!(store.sessions().expect("sessions").len(), 1);
    }

    #[test]
    fn an_unlocked_read_of_a_store_that_changed_is_retried() {
        let dir = tempfile::tempdir().expect("tempdir");
        let path = wal_store(dir.path());

        let store = Store::open(&path).expect("open");
        assert!(store.unlocked.is_some(), "no side files: read unlocked");
        assert_eq!(store.sessions().expect("sessions").len(), 1);
        store.finish().expect("nothing changed");

        // A writer starts (creating -wal and -shm) and commits during the read.
        let store = Store::open(&path).expect("open");
        assert_eq!(store.sessions().expect("sessions").len(), 1);
        let writer = Connection::open(&path).expect("writer");
        writer
            .execute("INSERT INTO session VALUES ('ses_b', 6)", [])
            .expect("write");
        let err = store.finish().expect_err("changed");
        assert!(
            matches!(&err, SourceError::Io(e) if e.kind() == io::ErrorKind::WouldBlock),
            "{err}"
        );
        drop(writer);

        // The main file rewritten (here: only its mtime) also counts.
        let store = Store::open(&path).expect("open");
        assert!(store.unlocked.is_some());
        let later = SystemTime::now() + Duration::from_secs(5);
        std::fs::File::options()
            .write(true)
            .open(&path)
            .and_then(|f| f.set_modified(later))
            .expect("touch");
        assert!(store.finish().is_err());

        // A locked read (through existing side files) is not checked this way.
        let writer = Connection::open(&path).expect("writer");
        writer
            .execute("INSERT INTO session VALUES ('ses_c', 7)", [])
            .expect("write");
        let store = Store::open(&path).expect("open");
        assert!(store.unlocked.is_none());
        assert_eq!(store.sessions().expect("sessions").len(), 3);
        store.finish().expect("locked reads need no check");
    }

    fn is_retry(err: &SourceError) -> bool {
        matches!(err, SourceError::Io(e) if e.kind() == io::ErrorKind::WouldBlock)
    }

    /// A malformed page met by an unlocked read means retry only if the store changed since it
    /// was opened; a damaged store that stays the same is unreadable, not retried forever.
    #[test]
    fn malformed_pages_mean_retry_only_if_an_unlocked_store_changed() {
        let dir = tempfile::tempdir().expect("tempdir");
        let path = wal_store(dir.path());
        // Junk after the first page: the schema still reads, the tables do not.
        let mut bytes = std::fs::read(&path).expect("read");
        assert!(bytes.len() > 8192);
        bytes[4096..].fill(0x5a);
        std::fs::write(&path, bytes).expect("write");

        // Damaged and unchanged: unreadable, every time.
        for _ in 0..2 {
            let store = Store::open(&path).expect("open");
            assert!(store.unlocked.is_some(), "no side files: read unlocked");
            let err = store.sessions().expect_err("malformed");
            assert!(matches!(&err, SourceError::Unreadable { .. }), "{err}");
        }

        // Changed between opening and the malformed page: retry.
        let store = Store::open(&path).expect("open");
        let later = SystemTime::now() + Duration::from_secs(5);
        std::fs::File::options()
            .write(true)
            .open(&path)
            .and_then(|f| f.set_modified(later))
            .expect("touch");
        let err = store.sessions().expect_err("malformed");
        assert!(is_retry(&err), "{err}");

        // A side file appearing (a writer starting) counts as a change too.
        let store = Store::open(&path).expect("open");
        std::fs::write(side_file(&path, "-wal"), b"").expect("wal");
        let err = store.sessions().expect_err("malformed");
        assert!(is_retry(&err), "{err}");
    }

    #[test]
    fn malformed_pages_in_a_locked_read_are_unreadable_and_busy_means_retry() {
        let failure = |code| rusqlite::Error::SqliteFailure(rusqlite::ffi::Error::new(code), None);
        let p = Path::new("x.db");
        assert!(matches!(
            sql_error(p, failure(rusqlite::ffi::SQLITE_CORRUPT), None),
            SourceError::Unreadable { .. }
        ));
        assert!(is_retry(&sql_error(
            p,
            failure(rusqlite::ffi::SQLITE_BUSY),
            None
        )));
        assert!(is_retry(&sql_error(
            p,
            failure(rusqlite::ffi::SQLITE_LOCKED),
            None
        )));
    }

    /// Listing sessions does not read parts: its work is the same for 10 parts as for 5000.
    #[test]
    fn listing_sessions_does_not_scan_parts() {
        let steps = |parts: usize| {
            let dir = tempfile::tempdir().expect("tempdir");
            let path = dir.path().join("opencode.db");
            let conn = Connection::open(&path).expect("open");
            conn.execute_batch(
                "CREATE TABLE session (id TEXT PRIMARY KEY, time_updated INTEGER);
                 CREATE TABLE message (id TEXT PRIMARY KEY, session_id TEXT, data TEXT);
                 CREATE TABLE part (id TEXT PRIMARY KEY, message_id TEXT, session_id TEXT, data TEXT);
                 CREATE INDEX part_session_idx ON part (session_id);
                 INSERT INTO session VALUES ('ses_a', 1), ('ses_b', 2);",
            )
            .expect("schema");
            let payload = "x".repeat(2000);
            conn.execute_batch("BEGIN").expect("begin");
            for i in 0..parts {
                let session = if i % 2 == 0 { "ses_a" } else { "ses_b" };
                conn.execute(
                    "INSERT INTO part VALUES (?1, 'msg', ?2, ?3)",
                    params![format!("prt_{i}"), session, payload],
                )
                .expect("insert");
            }
            conn.execute_batch("COMMIT").expect("commit");
            drop(conn);
            let store = Store::open(&path).expect("open");
            let listed = store.sessions().expect("sessions");
            assert_eq!(listed.len(), 2);
            assert!(listed.iter().all(|(_, change)| *change > 0));
            let mut stmt = store.conn.prepare(&store.sessions_sql()).expect("sql");
            let mut rows = stmt.query([]).expect("query");
            while rows.next().expect("row").is_some() {}
            drop(rows);
            stmt.get_status(StatementStatus::VmStep)
        };
        let (few, many) = (steps(10), steps(5000));
        assert!(
            many <= few + 10,
            "{few} VM steps for 10 parts, {many} for 5000"
        );
    }
}
