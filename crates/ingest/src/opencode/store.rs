//! Read-only access to an OpenCode SQLite store.
//!
//! The database is opened with `SQLITE_OPEN_READ_ONLY`, so SQLite never takes a write lock and
//! never creates the file. A rollback-journal store is read under a shared lock and nothing is
//! created next to it. While OpenCode has a WAL-mode store open, it is read through the existing
//! `-wal` and `-shm`; when those are gone, it is opened `immutable` (see [`quiet_wal_uri`]),
//! since a read-only connection would create them. A reader that meets a writer's lock waits at
//! most [`BUSY_TIMEOUT`], then fails with [`io::ErrorKind::WouldBlock`] so the caller retries
//! later.
//!
//! Columns are looked up first: a store from an older or newer OpenCode version with missing
//! optional columns still reads, and one missing a required table or column is reported as
//! unreadable rather than panicking.

use pitcrew_interfaces::source::SourceError;
use rusqlite::types::ValueRef;
use rusqlite::{Connection, ErrorCode, OpenFlags, OptionalExtension, Row, params};
use std::collections::HashSet;
use std::io;
use std::path::{Path, PathBuf};
use std::time::Duration;

/// How long a read waits for a writer's lock before giving up.
pub(crate) const BUSY_TIMEOUT: Duration = Duration::from_millis(250);

/// Ids longer than this are ignored as implausible (real ids are 30 bytes).
const MAX_ROW_ID_BYTES: usize = 256;

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

/// What a message's payload says, read by SQLite's JSON functions so large payloads are never
/// copied out.
#[derive(Clone, Debug, Default)]
pub(crate) struct MessageFacts {
    pub role: Option<String>,
    pub completed: bool,
    pub failed: bool,
    pub model: Option<String>,
}

/// The columns of the tables the adapter reads.
#[derive(Debug, Default)]
struct Schema {
    session: HashSet<String>,
    message: HashSet<String>,
    part: HashSet<String>,
}

/// An open, read-only store, inside one read transaction so every query sees the same snapshot.
pub(crate) struct Store {
    conn: Connection,
    path: PathBuf,
    schema: Schema,
    part_rowid: bool,
}

impl Store {
    /// Opens `path` read-only.
    pub(crate) fn open(path: &Path) -> Result<Self, SourceError> {
        let err = |e| sql_error(path, e);
        let flags = OpenFlags::SQLITE_OPEN_READ_ONLY
            | OpenFlags::SQLITE_OPEN_NO_MUTEX
            | OpenFlags::SQLITE_OPEN_URI;
        let conn = match quiet_wal_uri(path)? {
            Some(uri) => Connection::open_with_flags(uri, flags),
            None => Connection::open_with_flags(path, flags),
        }
        .map_err(err)?;
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
        })
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
        sql_error(&self.path, e)
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

    /// Every session with its size (bytes of part payloads).
    pub(crate) fn sessions(&self) -> Result<Vec<(SessionRow, u64)>, SourceError> {
        let sql = format!(
            "SELECT s.id, {parent}, {updated}, {size} FROM session s",
            parent = col(&self.schema.session, "s.parent_id", "parent_id", "NULL"),
            updated = col(&self.schema.session, "s.time_updated", "time_updated", "0"),
            size = if self.schema.part.contains("session_id") && self.schema.part.contains("data") {
                "(SELECT SUM(octet_length(p.data)) FROM part p WHERE p.session_id = s.id)"
            } else {
                "0"
            },
        );
        let mut stmt = self.conn.prepare(&sql).map_err(|e| self.err(e))?;
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
            let (id, mut session, size) = row.map_err(|e| self.err(e))?;
            let Some(id) = id.filter(|id| id.len() <= MAX_ROW_ID_BYTES) else {
                continue;
            };
            session.id = id;
            out.push((session, size));
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
            let ok = |s: &Option<String>| s.as_ref().is_some_and(|s| s.len() <= MAX_ROW_ID_BYTES);
            if let (true, true, Some(id), Some(message_id)) =
                (ok(&id), ok(&message_id), id, message_id)
            {
                out.push(PartRow {
                    id,
                    message_id,
                    created,
                    updated,
                    size: size.map(|n| u64::try_from(n).unwrap_or(0)),
                });
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
        let p = &self.schema.part;
        let sql = format!(
            "SELECT rowid, id, message_id, {}, {}, octet_length(data) FROM part
             WHERE session_id = ?1 AND rowid < ?2 ORDER BY rowid DESC LIMIT ?3",
            col(p, "time_created", "time_created", "0"),
            col(p, "time_updated", "time_updated", "0"),
        );
        let mut stmt = self.conn.prepare_cached(&sql).map_err(|e| self.err(e))?;
        let limit = i64::try_from(limit).unwrap_or(i64::MAX);
        let rows = stmt
            .query_map(params![session, below.unwrap_or(i64::MAX), limit], |row| {
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
            let row = match (id, message_id) {
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
            };
            out.push((rowid, row));
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

    /// The session's newest message.
    pub(crate) fn newest_message(&self, session: &str) -> Result<Option<MessageRow>, SourceError> {
        let order = col(
            &self.schema.message,
            "time_created",
            "time_created",
            "rowid",
        );
        let sql = format!(
            "SELECT id, {} FROM message WHERE session_id = ?1 ORDER BY {order} DESC, id DESC LIMIT 1",
            col(&self.schema.message, "time_created", "time_created", "0"),
        );
        self.cached_row(&sql, params![session], |row| {
            Ok(text(row, 0).map(|id| MessageRow {
                id,
                created: int(row, 1),
            }))
        })
        .optional()
        .map(Option::flatten)
        .map_err(|e| self.err(e))
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

    /// What one message's payload says. Unreadable payloads give empty facts.
    pub(crate) fn message_facts(&self, id: &str) -> Result<MessageFacts, SourceError> {
        const SQL: &str = "SELECT
            CASE WHEN json_type(d, '$.role') = 'text' THEN json_extract(d, '$.role') END,
            json_type(d, '$.time.completed') IS NOT NULL AND json_type(d, '$.time.completed') != 'null',
            json_type(d, '$.error') IS NOT NULL AND json_type(d, '$.error') != 'null',
            CASE WHEN json_type(d, '$.modelID') = 'text' THEN json_extract(d, '$.modelID') END
            FROM (SELECT CASE WHEN typeof(data) = 'text' AND json_valid(data) THEN data END AS d
                  FROM message WHERE id = ?1)";
        self.cached_row(SQL, params![id], |row| {
            Ok(MessageFacts {
                role: text(row, 0),
                completed: int(row, 1) != 0,
                failed: int(row, 2) != 0,
                model: text(row, 3),
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

/// For a WAL-mode store whose `-wal` or `-shm` file is missing (OpenCode is not running), an
/// `immutable` URI: a read-only connection would otherwise create those files. With both present,
/// `None`, and the store is read through them like any WAL reader.
///
/// `immutable` skips locking, which is safe while nothing writes. If OpenCode starts during the
/// read, its writes go to a new `-wal`; only a checkpoint rewrites the main file, which a
/// read of milliseconds is very unlikely to meet, and a torn read fails with an error and is
/// retried. A `-wal` left without its `-shm` by a crash is not read until OpenCode recovers it.
fn quiet_wal_uri(path: &Path) -> Result<Option<String>, SourceError> {
    use std::io::Read;
    let mut header = [0u8; 20];
    let n = match std::fs::File::open(path).and_then(|mut f| f.read(&mut header)) {
        Ok(n) => n,
        // Let SQLite report a missing or unreadable file.
        Err(_) => return Ok(None),
    };
    let wal_mode = n == header.len() && (header[18] == 2 || header[19] == 2);
    let side = |suffix: &str| {
        let mut name = path.as_os_str().to_owned();
        name.push(suffix);
        PathBuf::from(name).exists()
    };
    if !wal_mode || (side("-wal") && side("-shm")) {
        return Ok(None);
    }
    let Some(text) = path.to_str() else {
        return Err(SourceError::Unreadable {
            path: path.to_path_buf(),
            reason: "the store's path is not UTF-8".into(),
        });
    };
    let mut plain = text.replace('\\', "/");
    if !plain.starts_with('/') {
        plain.insert(0, '/');
    }
    let mut uri = String::from("file://");
    for b in plain.bytes() {
        if b.is_ascii_alphanumeric() || b"/-._~:".contains(&b) {
            uri.push(char::from(b));
        } else {
            uri.push_str(&format!("%{b:02X}"));
        }
    }
    uri.push_str("?mode=ro&immutable=1");
    Ok(Some(uri))
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

/// A busy or locked store is a retry-later I/O error; anything else makes it unreadable.
pub(crate) fn sql_error(path: &Path, e: rusqlite::Error) -> SourceError {
    match e.sqlite_error_code() {
        Some(ErrorCode::DatabaseBusy | ErrorCode::DatabaseLocked) => {
            SourceError::Io(io::Error::new(
                io::ErrorKind::WouldBlock,
                format!("{} is locked by a writer; retry later", path.display()),
            ))
        }
        _ => SourceError::Unreadable {
            path: path.to_path_buf(),
            reason: e.to_string(),
        },
    }
}
