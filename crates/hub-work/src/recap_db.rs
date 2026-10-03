//! Where the recap index keeps its blocks: a SQLite database of its own, apart from the hub's
//! store, in memory or in a file.
//!
//! - **One row a block**, its JSON as the body, with the links queries filter by (session,
//!   workstream, project, and the tasks in a table of their own), and its start and last event,
//!   for days. Ids are 16-byte big-endian blobs, so SQLite orders them as [`EventId`] does.
//! - **Days read bodies only to write a paragraph.** A day query reads its blocks' heads
//!   ([`Head`]: place, workstream, last event) from an index that holds them, which is all a
//!   cached paragraph needs to be checked; bodies are read and decoded only when their paragraph
//!   is written again ([`BlockDb::bodies`]).
//! - **A cache, never the truth.** The file is replaced whenever an index opens it, and removed
//!   when the index is dropped: the blocks are derived from the log, which the index reads from
//!   its start each time it is built (see [`crate::recap`]). A crash leaves a file that the next
//!   start replaces, never one that is read again.
//! - **On a local disk.** A cache file asked for on a network filesystem (detected as the store
//!   detects its own, `pitcrew_store::detect`) goes in a private folder of its own on a local disk
//!   instead: in the runtime directory (`$XDG_RUNTIME_DIR`) or the temporary folder; with
//!   neither, the blocks stay in memory ([`BlockDb::local`]).
//! - **Writes are all or nothing.** [`BlockDb::put`] writes one batch of changed blocks in one
//!   transaction. A batch that fails leaves the database where it was, which is then behind the
//!   index's engine: the index counts as broken, and is rebuilt (see [`crate::Recaps`]).
//! - **Exact.** A block comes back as it went in (`serde_json` round trip, which the tests
//!   check), and the queries keep the order of the in-memory sets they replace: blocks by id,
//!   newest first, and a scope's blocks by `(start, id)`.

use crate::error::{Result, WorkError};
use crate::recap::BlockFilter;
use pitcrew_protocol::ids::{EventId, WorkstreamId};
use pitcrew_protocol::model::TimestampMs;
use pitcrew_protocol::recap::Block;
use pitcrew_store::sql::types::Value;
use pitcrew_store::sql::{Connection, OptionalExtension, params, params_from_iter};
use std::ffi::OsStr;
use std::fs;
use std::io;
use std::path::{Path, PathBuf};

/// The tables. Partial indexes, one per filter, so each query walks only its own blocks: by id
/// for `GET /v1/recaps/blocks`, by `(start, id)` for days. The days' indexes also hold what a
/// day query reads of a block before its body (its workstream and last event), so it reads them
/// alone. `last` comes before `body` in the row, so reading it never walks a long body's pages.
const SCHEMA: &str = "
CREATE TABLE blocks (
    id BLOB NOT NULL PRIMARY KEY,
    start INTEGER NOT NULL,
    last BLOB NOT NULL,
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
CREATE INDEX blocks_workstream_days ON blocks (workstream, start, id, last)
    WHERE workstream IS NOT NULL;
CREATE INDEX blocks_project ON blocks (project, id) WHERE project IS NOT NULL;
CREATE INDEX blocks_project_days ON blocks (project, start, id, workstream, last)
    WHERE project IS NOT NULL;
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

/// What a day query reads of a block before its body: enough to group it by day and workstream,
/// and to tell whether a cached paragraph was written from it as it is now.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct Head {
    pub id: EventId,
    pub start: TimestampMs,
    pub workstream: Option<WorkstreamId>,
    /// Its last event: the block has changed when this has.
    pub last: EventId,
}

/// The blocks of one recap index. See the [module docs](self).
pub(crate) struct BlockDb {
    /// Declared before `file`, so the database is closed before its file is removed.
    conn: Connection,
    /// Declared before `folder`, so the file is removed before its folder.
    _file: Option<OwnedFile>,
    /// The private folder the file was put in, when its own was on a network filesystem.
    _folder: Option<OwnedFolder>,
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

/// A private folder this process created for the recap file, removed when dropped (once the
/// file in it is gone).
struct OwnedFolder(PathBuf);

impl OwnedFolder {
    /// A new folder in `base` that only this user may open (0700 on Unix), with a name no one
    /// can guess beforehand. `base` must not let others rename what is in it (on Unix: not
    /// writable by group or others, unless sticky, as `/tmp` is).
    fn make(base: &Path) -> io::Result<Self> {
        let meta = fs::metadata(base)?;
        if !meta.is_dir() {
            return Err(io::Error::other("not a folder"));
        }
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt as _;
            let mode = meta.permissions().mode();
            if mode & 0o022 != 0 && mode & 0o1000 == 0 {
                return Err(io::Error::other(format!(
                    "others can change what is in it (mode {:o})",
                    mode & 0o7777
                )));
            }
        }
        let path = base.join(format!("pitcrew-recaps-{}", EventId::new().0));
        let mut builder = fs::DirBuilder::new();
        #[cfg(unix)]
        {
            use std::os::unix::fs::DirBuilderExt as _;
            builder.mode(0o700);
        }
        // Never there before: `create` fails on anything at the path, a link included.
        builder.create(&path)?;
        let folder = Self(path);
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt as _;
            fs::set_permissions(&folder.0, fs::Permissions::from_mode(0o700))?;
        }
        Ok(folder)
    }
}

impl Drop for OwnedFolder {
    fn drop(&mut self) {
        if let Err(e) = fs::remove_dir(&self.0)
            && e.kind() != io::ErrorKind::NotFound
        {
            tracing::warn!(folder = %self.0.display(), error = %e, "cannot remove the recap index's folder");
        }
    }
}

/// The folder a file at `path` is in (`.` for a bare name).
fn folder_of(path: &Path) -> PathBuf {
    path.parent()
        .filter(|p| !p.as_os_str().is_empty())
        .map_or_else(|| PathBuf::from("."), Path::to_path_buf)
}

/// Where a private folder for the recap file may be made, in order: the user's runtime
/// directory (`$XDG_RUNTIME_DIR`, when set to an absolute path) and the temporary folder.
fn local_bases() -> Vec<PathBuf> {
    let mut bases: Vec<PathBuf> = std::env::var_os("XDG_RUNTIME_DIR")
        .map(PathBuf::from)
        .filter(|dir| dir.is_absolute())
        .into_iter()
        .collect();
    bases.push(std::env::temp_dir());
    bases
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

    /// An empty database for a cache file meant for `path`, kept on a local disk: at `path` when
    /// its folder is on one; when that is on a network filesystem (or one not recognised, as the
    /// store treats it), in a new private folder of its own in the runtime directory or the
    /// temporary folder ([`BlockDb::file`] there); in memory when neither can be had. Logs where
    /// it went when that is not `path`.
    pub fn local(path: &Path) -> Result<Self> {
        Self::local_with(
            path,
            &|dir: &Path| pitcrew_store::detect(dir).is_network(),
            &local_bases(),
        )
    }

    /// [`BlockDb::local`], with whether a folder is on a network filesystem (`network`) and where
    /// a private folder may be made (`bases`, in order) given.
    fn local_with(path: &Path, network: &dyn Fn(&Path) -> bool, bases: &[PathBuf]) -> Result<Self> {
        if !network(&folder_of(path)) {
            return Self::file(path);
        }
        let name = path
            .file_name()
            .unwrap_or_else(|| OsStr::new("recaps.sqlite3"));
        for base in bases {
            if network(base) {
                tracing::debug!(folder = %base.display(), "not a local disk, so not for the recap index's file");
                continue;
            }
            let folder = match OwnedFolder::make(base) {
                Ok(folder) => folder,
                Err(e) => {
                    tracing::debug!(folder = %base.display(), error = %e, "cannot make a private folder for the recap index's file there");
                    continue;
                }
            };
            let file = folder.0.join(name);
            match Self::file(&file) {
                Ok(mut db) => {
                    tracing::info!(
                        asked = %path.display(),
                        file = %file.display(),
                        "the recap index's file would be on a network filesystem, so it is in a \
                         private folder on a local disk instead"
                    );
                    db._folder = Some(folder);
                    return Ok(db);
                }
                Err(e) => {
                    tracing::debug!(file = %file.display(), error = %e, "cannot make the recap index's file there");
                }
            }
        }
        tracing::warn!(
            asked = %path.display(),
            "the recap index's file would be on a network filesystem, and no private folder on a \
             local disk could be made: the recap index keeps its blocks in memory"
        );
        Self::memory()
    }

    fn init(conn: Connection, file: Option<OwnedFile>) -> Result<Self> {
        conn.pragma_update(None, "temp_store", "MEMORY")?;
        conn.execute_batch(SCHEMA)?;
        Ok(Self {
            conn,
            _file: file,
            _folder: None,
            count: 0,
        })
    }

    /// Whether the blocks are in a file (not in memory). For tests.
    #[cfg(test)]
    pub fn file_path(&self) -> Option<&Path> {
        self._file.as_ref().map(|f| f.0.as_path())
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
                "INSERT INTO blocks (id, start, last, session, workstream, project, tasks, body)
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8)",
            )?;
            let mut update = tx.prepare_cached(
                "UPDATE blocks SET start = ?2, last = ?3, session = ?4, workstream = ?5,
                    project = ?6, tasks = ?7, body = ?8
                 WHERE id = ?1",
            )?;
            let mut link =
                tx.prepare_cached("INSERT OR IGNORE INTO block_tasks (task, id) VALUES (?1, ?2)")?;
            let mut unlink =
                tx.prepare_cached("DELETE FROM block_tasks WHERE task = ?1 AND id = ?2")?;
            for block in blocks {
                let id = key(block.id.0);
                let last = key(block.last.0);
                let tasks: Vec<u8> = block.tasks.iter().flat_map(|t| key(t.0)).collect();
                let body = serde_json::to_string(block)?;
                let session = block.session.map(|s| key(s.0).to_vec());
                let workstream = block.workstream.map(|w| key(w.0).to_vec());
                let project = block.project.map(|p| key(p.0).to_vec());
                let values = params![
                    &id[..],
                    block.start,
                    &last[..],
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

    /// The heads of `scope`'s blocks from `low` to `high`, both included, latest first: read
    /// from the scope's days index alone, without a body.
    pub fn window(&self, scope: Scope, low: Place, high: Place) -> Result<Vec<Head>> {
        let (column, id) = scope.column();
        let mut stmt = self.conn.prepare_cached(&format!(
            "SELECT id, start, workstream, last FROM blocks
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
            |r| {
                Ok((
                    r.get::<_, Vec<u8>>(0)?,
                    r.get::<_, i64>(1)?,
                    r.get::<_, Option<Vec<u8>>>(2)?,
                    r.get::<_, Vec<u8>>(3)?,
                ))
            },
        )?;
        let mut heads = Vec::new();
        for row in rows {
            let (id, start, workstream, last) = row?;
            heads.push(Head {
                id: EventId(ulid::Ulid(from_key(&id)?)),
                start,
                workstream: workstream
                    .map(|w| from_key(&w).map(|w| WorkstreamId(ulid::Ulid(w))))
                    .transpose()?,
                last: EventId(ulid::Ulid(from_key(&last)?)),
            });
        }
        Ok(heads)
    }

    /// The blocks of `scope` from `low` to `high`, both included, whose workstream is
    /// `workstream`, oldest first: the blocks of one paragraph, found as [`BlockDb::window`] found
    /// their heads.
    pub fn bodies(
        &self,
        scope: Scope,
        low: Place,
        high: Place,
        workstream: Option<WorkstreamId>,
    ) -> Result<Vec<Block>> {
        let (column, id) = scope.column();
        let mut stmt = self.conn.prepare_cached(&format!(
            "SELECT body FROM blocks
             WHERE {column} = ?1 AND (start, id) >= (?2, ?3) AND (start, id) <= (?4, ?5)
                AND workstream IS ?6
             ORDER BY start, id"
        ))?;
        let rows = stmt.query_map(
            params![
                &id[..],
                low.0,
                &key(low.1.0)[..],
                high.0,
                &key(high.1.0)[..],
                workstream.map(|w| key(w.0).to_vec()),
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
    #[cfg(test)]
    tests::DECODED.with(|n| n.set(n.get() + 1));
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

#[cfg(test)]
pub(crate) mod tests {
    use super::*;
    use std::cell::Cell;

    thread_local! {
        /// Block bodies decoded on this thread.
        pub(crate) static DECODED: Cell<usize> = const { Cell::new(0) };
    }

    /// Block bodies decoded on this thread so far.
    pub(crate) fn decoded() -> usize {
        DECODED.with(Cell::get)
    }

    fn entries(dir: &Path) -> Vec<PathBuf> {
        let mut found: Vec<PathBuf> = fs::read_dir(dir)
            .expect("read_dir")
            .map(|e| e.expect("entry").path())
            .collect();
        found.sort();
        found
    }

    #[cfg(unix)]
    fn mode(path: &Path) -> u32 {
        use std::os::unix::fs::PermissionsExt as _;
        fs::metadata(path).expect("meta").permissions().mode() & 0o7777
    }

    /// On a local disk, the file is where it was asked for.
    #[test]
    fn on_a_local_disk_the_file_is_where_it_was_asked() {
        let state = tempfile::tempdir().expect("tempdir");
        let base = tempfile::tempdir().expect("tempdir");
        let asked = state.path().join("recaps.sqlite3");
        let db = BlockDb::local_with(&asked, &|_: &Path| false, &[base.path().to_path_buf()])
            .expect("db");
        assert_eq!(db.file_path(), Some(asked.as_path()));
        assert!(asked.is_file());
        assert!(entries(base.path()).is_empty());
        drop(db);
        assert!(!asked.exists());
    }

    /// When the state directory is on a network filesystem (detection stubbed), the file goes
    /// in a new private folder in the first local base, never at the path asked for; a base on a
    /// network filesystem too is skipped. The folder goes with the database.
    #[test]
    fn on_a_network_filesystem_the_file_is_in_a_private_local_folder() {
        let state = tempfile::tempdir().expect("tempdir");
        let remote = tempfile::tempdir().expect("tempdir");
        let local = tempfile::tempdir().expect("tempdir");
        let asked = state.path().join("recaps.sqlite3");
        let network = |dir: &Path| dir == state.path() || dir == remote.path();
        let bases = [remote.path().to_path_buf(), local.path().to_path_buf()];
        let mut db = BlockDb::local_with(&asked, &network, &bases).expect("db");

        assert!(!asked.exists(), "nothing on the network filesystem");
        assert!(entries(state.path()).is_empty());
        assert!(entries(remote.path()).is_empty());
        let made = entries(local.path());
        assert_eq!(made.len(), 1, "{made:?}");
        let folder = &made[0];
        assert!(
            folder
                .file_name()
                .and_then(OsStr::to_str)
                .is_some_and(|n| n.starts_with("pitcrew-recaps-")),
            "{folder:?}"
        );
        let file = folder.join("recaps.sqlite3");
        assert_eq!(db.file_path(), Some(file.as_path()));
        assert_eq!(entries(folder), vec![file.clone()]);
        #[cfg(unix)]
        {
            assert_eq!(mode(folder), 0o700, "the folder is private");
            assert_eq!(mode(&file), 0o600, "the file is private");
        }
        // A working database.
        db.put(&[]).expect("put");
        assert_eq!(
            db.page(&BlockFilter::default(), None, 10).expect("page"),
            (Vec::new(), false)
        );

        drop(db);
        assert!(!folder.exists(), "the folder goes with the database");
        assert!(entries(local.path()).is_empty());
    }

    /// With no local base (each one on a network filesystem, or one it cannot use), the blocks
    /// stay in memory, and nothing is left anywhere.
    #[test]
    fn with_no_private_local_folder_the_blocks_stay_in_memory() {
        let state = tempfile::tempdir().expect("tempdir");
        let base = tempfile::tempdir().expect("tempdir");
        let asked = state.path().join("recaps.sqlite3");
        let bases = [base.path().to_path_buf(), base.path().join("missing")];
        let db = BlockDb::local_with(&asked, &|_: &Path| true, &bases).expect("db");
        assert_eq!(db.file_path(), None);
        let only_state = |dir: &Path| dir == state.path();
        let missing = [base.path().join("missing")];
        let also = BlockDb::local_with(&asked, &only_state, &missing).expect("db");
        assert_eq!(also.file_path(), None);
        assert!(entries(state.path()).is_empty());
        assert!(entries(base.path()).is_empty());
    }

    /// A base whose entries others could rename (writable by group or others, and not sticky) is
    /// not used; one that is sticky, as `/tmp` is, is.
    #[cfg(unix)]
    #[test]
    fn a_base_others_can_change_is_not_used() {
        use std::os::unix::fs::PermissionsExt as _;
        let state = tempfile::tempdir().expect("tempdir");
        let open = tempfile::tempdir().expect("tempdir");
        let sticky = tempfile::tempdir().expect("tempdir");
        fs::set_permissions(open.path(), fs::Permissions::from_mode(0o777)).expect("chmod");
        fs::set_permissions(sticky.path(), fs::Permissions::from_mode(0o1777)).expect("chmod");
        let asked = state.path().join("recaps.sqlite3");
        let network = |dir: &Path| dir == state.path();
        let bases = [open.path().to_path_buf(), sticky.path().to_path_buf()];
        let db = BlockDb::local_with(&asked, &network, &bases).expect("db");
        assert!(entries(open.path()).is_empty());
        let made = entries(sticky.path());
        assert_eq!(made.len(), 1);
        assert_eq!(db.file_path(), Some(made[0].join("recaps.sqlite3").as_path()));
        assert_eq!(mode(&made[0]), 0o700);
    }
}
