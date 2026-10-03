//! Where the recap index keeps its blocks: a SQLite database of its own, apart from the hub's
//! store, in memory or in a file.
//!
//! - **One row a block**, its JSON as the body, with the links queries filter by (session,
//!   workstream, project, and the tasks in a table of their own) and its start, for days. Ids are
//!   16-byte big-endian blobs, so SQLite orders them as [`EventId`] does.
//! - **A cache, never the truth.** The file is replaced whenever an index opens it, and removed
//!   when the index is dropped: the blocks are derived from the log, which the index reads from
//!   its start each time it is built (see [`crate::recap`]). A crash leaves a file that the next
//!   start replaces, never one that is read again.
//! - **Writes are all or nothing.** [`BlockDb::put`] writes one batch of changed blocks in one
//!   transaction. A batch that fails leaves the database where it was, which is then behind the
//!   index's engine: the index counts as broken, and is rebuilt (see [`crate::Recaps`]).
//! - **Exact.** A block comes back as it went in (`serde_json` round trip, which the tests
//!   check), and the queries keep the order of the in-memory sets they replace: blocks by id,
//!   newest first, and a scope's blocks by `(start, id)`.

use crate::error::{Result, WorkError};
use crate::recap::BlockFilter;
use pitcrew_protocol::ids::EventId;
use pitcrew_protocol::model::TimestampMs;
use pitcrew_protocol::recap::Block;
use pitcrew_store::sql::types::Value;
use pitcrew_store::sql::{Connection, OptionalExtension, params, params_from_iter};
use std::fs;
use std::io;
use std::path::{Path, PathBuf};

/// The tables. Partial indexes, one per filter, so each query walks only its own blocks: by id
/// for `GET /v1/recaps/blocks`, by `(start, id)` for days.
const SCHEMA: &str = "
CREATE TABLE blocks (
    id BLOB NOT NULL PRIMARY KEY,
    start INTEGER NOT NULL,
    session BLOB,
    workstream BLOB,
    project BLOB,
    tasks BLOB NOT NULL,
    body TEXT NOT NULL
);
CREATE TABLE block_tasks (
    task BLOB NOT NULL,
    id BLOB NOT NULL,
    PRIMARY KEY (task, id)
) WITHOUT ROWID;
CREATE INDEX blocks_session ON blocks (session, id) WHERE session IS NOT NULL;
CREATE INDEX blocks_workstream ON blocks (workstream, id) WHERE workstream IS NOT NULL;
CREATE INDEX blocks_workstream_days ON blocks (workstream, start, id)
    WHERE workstream IS NOT NULL;
CREATE INDEX blocks_project ON blocks (project, id) WHERE project IS NOT NULL;
CREATE INDEX blocks_project_days ON blocks (project, start, id) WHERE project IS NOT NULL;
";

/// The page cache of a file database, in KiB (SQLite's own default is 2,000).
const FILE_CACHE_KIB: i64 = 1024;

/// Whose blocks a day query reads: one workstream's or one project's.
#[derive(Clone, Copy, Debug)]
pub(crate) enum Scope {
    Workstream([u8; 16]),
    Project([u8; 16]),
}

impl Scope {
    fn column(self) -> (&'static str, [u8; 16]) {
        match self {
            Self::Workstream(id) => ("workstream", id),
            Self::Project(id) => ("project", id),
        }
    }
}

/// A block's place among a scope's: its start, then its id.
pub(crate) type Place = (TimestampMs, EventId);

/// The blocks of one recap index. See the [module docs](self).
pub(crate) struct BlockDb {
    /// Declared before `file`, so the database is closed before its file is removed.
    conn: Connection,
    _file: Option<OwnedFile>,
    /// Blocks stored.
    count: usize,
}

impl std::fmt::Debug for BlockDb {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("BlockDb")
            .field("blocks", &self.count)
            .field("file", &self._file.as_ref().map(|f| &f.0))
            .finish()
    }
}

/// A file this process created and removes when dropped.
struct OwnedFile(PathBuf);

impl Drop for OwnedFile {
    fn drop(&mut self) {
        if let Err(e) = fs::remove_file(&self.0)
            && e.kind() != io::ErrorKind::NotFound
        {
            tracing::warn!(file = %self.0.display(), error = %e, "cannot remove the recap index's file");
        }
    }
}

impl BlockDb {
    /// An empty database in memory.
    pub fn memory() -> Result<Self> {
        Self::init(Connection::open_in_memory()?, None)
    }

    /// An empty database in a file at `path`, replacing whatever is there; removed when dropped.
    /// The file is private to the user (0600 on Unix): blocks hold titles, paths and text.
    pub fn file(path: &Path) -> Result<Self> {
        match fs::remove_file(path) {
            Ok(()) => {}
            Err(e) if e.kind() == io::ErrorKind::NotFound => {}
            Err(e) => {
                return Err(WorkError::internal(format!(
                    "cannot replace the recap index's file {}: {e}",
                    path.display()
                )));
            }
        }
        create_private(path).map_err(|e| {
            WorkError::internal(format!(
                "cannot create the recap index's file {}: {e}",
                path.display()
            ))
        })?;
        let owned = OwnedFile(path.to_path_buf());
        let conn = Connection::open(path)?;
        // One process uses it, and a crash discards it: no shared lock, no journal on disk, no
        // fsync. A transaction still rolls back (the journal is in memory).
        conn.pragma_update(None, "locking_mode", "EXCLUSIVE")?;
        conn.pragma_update(None, "journal_mode", "MEMORY")?;
        conn.pragma_update(None, "synchronous", "OFF")?;
        conn.pragma_update(None, "cache_size", -FILE_CACHE_KIB)?;
        Self::init(conn, Some(owned))
    }

    fn init(conn: Connection, file: Option<OwnedFile>) -> Result<Self> {
        conn.pragma_update(None, "temp_store", "MEMORY")?;
        conn.execute_batch(SCHEMA)?;
        Ok(Self {
            conn,
            _file: file,
            count: 0,
        })
    }

    /// How many blocks are stored.
    pub fn len(&self) -> usize {
        self.count
    }

    /// Stores blocks that began or changed, in one transaction: each replaces the one with its id,
    /// and is linked to what it names now.
    pub fn put(&mut self, blocks: &[Block]) -> Result<()> {
        if blocks.is_empty() {
            return Ok(());
        }
        let tx = self.conn.transaction()?;
        let mut added = 0;
        {
            let mut old = tx.prepare_cached("SELECT tasks FROM blocks WHERE id = ?1")?;
            let mut insert = tx.prepare_cached(
                "INSERT INTO blocks (id, start, session, workstream, project, tasks, body)
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7)",
            )?;
            let mut update = tx.prepare_cached(
                "UPDATE blocks SET start = ?2, session = ?3, workstream = ?4, project = ?5,
                    tasks = ?6, body = ?7
                 WHERE id = ?1",
            )?;
            let mut link =
                tx.prepare_cached("INSERT OR IGNORE INTO block_tasks (task, id) VALUES (?1, ?2)")?;
            let mut unlink =
                tx.prepare_cached("DELETE FROM block_tasks WHERE task = ?1 AND id = ?2")?;
            for block in blocks {
                let id = key(block.id.0);
                let tasks: Vec<u8> = block.tasks.iter().flat_map(|t| key(t.0)).collect();
                let body = serde_json::to_string(block)?;
                let session = block.session.map(|s| key(s.0).to_vec());
                let workstream = block.workstream.map(|w| key(w.0).to_vec());
                let project = block.project.map(|p| key(p.0).to_vec());
                let values = params![
                    &id[..],
                    block.start,
                    session,
                    workstream,
                    project,
                    tasks,
                    body,
                ];
                let before: Option<Vec<u8>> = old.query_row([&id[..]], |r| r.get(0)).optional()?;
                match before {
                    None => {
                        insert.execute(values)?;
                        added += 1;
                    }
                    Some(before) => {
                        update.execute(values)?;
                        if before == tasks {
                            continue;
                        }
                        let now: &[[u8; 16]] = tasks.as_chunks().0;
                        for task in before.as_chunks::<16>().0 {
                            if !now.contains(task) {
                                unlink.execute(params![&task[..], &id[..]])?;
                            }
                        }
                    }
                }
                for task in tasks.as_chunks::<16>().0 {
                    link.execute(params![&task[..], &id[..]])?;
                }
            }
        }
        tx.commit()?;
        self.count += added;
        Ok(())
    }

    /// Up to `limit` blocks linked to all of `filter`, older than `before`, newest first, and
    /// whether an older one also matches.
    pub fn page(
        &self,
        filter: &BlockFilter,
        before: Option<EventId>,
        limit: usize,
    ) -> Result<(Vec<Block>, bool)> {
        let mut sql = String::from("SELECT body FROM blocks WHERE 1");
        let mut values: Vec<Value> = Vec::new();
        let mut and = |condition: &str, id: Option<ulid::Ulid>| {
            if let Some(id) = id {
                sql.push_str(" AND ");
                sql.push_str(condition);
                values.push(Value::Blob(key(id).to_vec()));
            }
        };
        and("session = ?", filter.session.map(|s| s.0));
        and("workstream = ?", filter.workstream.map(|w| w.0));
        and("project = ?", filter.project.map(|p| p.0));
        and(
            "id IN (SELECT id FROM block_tasks WHERE task = ?)",
            filter.task.map(|t| t.0),
        );
        and("id < ?", before.map(|b| b.0));
        sql.push_str(" ORDER BY id DESC LIMIT ?");
        values.push(Value::Integer(
            i64::try_from(limit.saturating_add(1)).unwrap_or(i64::MAX),
        ));
        let mut stmt = self.conn.prepare_cached(&sql)?;
        let mut blocks = Vec::new();
        let mut more = false;
        let mut rows = stmt.query(params_from_iter(values))?;
        while let Some(row) = rows.next()? {
            if blocks.len() == limit {
                more = true;
                break;
            }
            blocks.push(decode(&row.get::<_, String>(0)?)?);
        }
        Ok((blocks, more))
    }

    /// The last place among `scope`'s blocks before `below` (all of them without it).
    pub fn latest(&self, scope: Scope, below: Option<Place>) -> Result<Option<Place>> {
        let (column, id) = scope.column();
        let row =
            |r: &pitcrew_store::sql::Row<'_>| Ok((r.get::<_, i64>(0)?, r.get::<_, Vec<u8>>(1)?));
        let found = match below {
            None => self
                .conn
                .prepare_cached(&format!(
                    "SELECT start, id FROM blocks WHERE {column} = ?1
                     ORDER BY start DESC, id DESC LIMIT 1"
                ))?
                .query_row(params![&id[..]], row)
                .optional()?,
            Some((start, below)) => self
                .conn
                .prepare_cached(&format!(
                    "SELECT start, id FROM blocks WHERE {column} = ?1 AND (start, id) < (?2, ?3)
                     ORDER BY start DESC, id DESC LIMIT 1"
                ))?
                .query_row(params![&id[..], start, &key(below.0)[..]], row)
                .optional()?,
        };
        found
            .map(|(start, id)| Ok((start, EventId(ulid::Ulid(from_key(&id)?)))))
            .transpose()
    }

    /// `scope`'s blocks from `low` to `high`, both included, latest first.
    pub fn window(&self, scope: Scope, low: Place, high: Place) -> Result<Vec<Block>> {
        let (column, id) = scope.column();
        let mut stmt = self.conn.prepare_cached(&format!(
            "SELECT body FROM blocks
             WHERE {column} = ?1 AND (start, id) >= (?2, ?3) AND (start, id) <= (?4, ?5)
             ORDER BY start DESC, id DESC"
        ))?;
        let rows = stmt.query_map(
            params![
                &id[..],
                low.0,
                &key(low.1.0)[..],
                high.0,
                &key(high.1.0)[..]
            ],
            |r| r.get::<_, String>(0),
        )?;
        let mut blocks = Vec::new();
        for body in rows {
            blocks.push(decode(&body?)?);
        }
        Ok(blocks)
    }
}

/// An id as stored: big-endian, so blobs sort as ids do.
pub(crate) fn key(id: ulid::Ulid) -> [u8; 16] {
    id.to_bytes()
}

fn from_key(bytes: &[u8]) -> Result<u128> {
    let bytes: [u8; 16] = bytes
        .try_into()
        .map_err(|_| WorkError::internal("a recap block id is not 16 bytes"))?;
    Ok(u128::from_be_bytes(bytes))
}

fn decode(body: &str) -> Result<Block> {
    Ok(serde_json::from_str(body)?)
}

/// Creates an empty file only this user may read.
fn create_private(path: &Path) -> io::Result<()> {
    let mut options = fs::OpenOptions::new();
    options.write(true).create_new(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt as _;
        options.mode(0o600);
    }
    options.open(path).map(drop)
}
