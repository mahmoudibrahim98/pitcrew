use crate::error::{Error, Result};
use crate::fs_kind::{self, FsMode};
use crate::lease::{Clock, LeaseGuard, SystemClock};
use crate::maintenance::{self, IntegrityReport};
use crate::migrations::{self, Migration};
use crate::projection::{self, Checkpoint, Projection};
use pitcrew_protocol::events::{Event, EventBody};
use pitcrew_protocol::ids::{EventId, MemberId, WorkspaceId};
use rusqlite::{Connection, OpenFlags, OptionalExtension, Row, Transaction, TransactionBehavior};
use std::collections::BTreeSet;
use std::io::{BufRead, Write};
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex, MutexGuard, PoisonError};
use std::time::Duration;
use tokio::sync::broadcast;

/// How to open a store.
#[derive(Debug, Clone)]
#[non_exhaustive]
pub struct StoreOptions {
    /// How long a writer waits for a lock held by another connection before failing.
    pub busy_timeout: Duration,
    /// How many revision ranges a slow subscriber may fall behind before it lags. Clamped to
    /// `1..=MAX_SUBSCRIBER_CAPACITY`.
    pub subscriber_capacity: usize,
    /// How to choose between WAL (local disks) and the NFS-safe journal mode plus a single-host
    /// lease (network filesystems). The default, `FsMode::Auto`, detects the filesystem of the
    /// database's directory; still picks WAL for a local disk, unchanged from before this option
    /// existed.
    pub fs: FsMode,
    /// How long the network-mode lease lasts before it may be taken over if nobody renews it.
    /// The owner renews it automatically, well before it would expire (see the crate's `lease`
    /// module). Ignored in local (WAL) mode.
    pub lease_ttl: Duration,
    /// Where the network-mode lease gets the time. The default is the system clock; tests inject
    /// one they control, so a lease can be made to look expired without sleeping.
    pub clock: Arc<dyn Clock>,
}

/// The largest `subscriber_capacity`; the channel allocates this many slots up front.
pub const MAX_SUBSCRIBER_CAPACITY: usize = 65_536;

impl Default for StoreOptions {
    fn default() -> Self {
        Self {
            busy_timeout: Duration::from_secs(5),
            subscriber_capacity: 1024,
            fs: FsMode::Auto,
            lease_ttl: Duration::from_secs(60),
            clock: Arc::new(SystemClock),
        }
    }
}

/// Revisions `from_rev..=to_rev`, as in `StreamFrame::Events`. Empty when `to_rev < from_rev`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct RevRange {
    /// First revision.
    pub from_rev: u64,
    /// Last revision.
    pub to_rev: u64,
}

impl RevRange {
    /// Whether the range holds no revisions.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.to_rev < self.from_rev
    }

    /// How many revisions it holds.
    #[must_use]
    pub fn len(&self) -> u64 {
        if self.is_empty() {
            0
        } else {
            (self.to_rev - self.from_rev).saturating_add(1)
        }
    }
}

/// An event with its revision.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct StoredEvent {
    /// The hub-local, gap-free revision.
    pub rev: u64,
    /// The event.
    pub event: Event,
}

/// Which events to return. Empty matches everything. More fields (project, workstream, task,
/// session) will be added; build it with `EventFilter::default()` and the methods.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
#[non_exhaustive]
pub struct EventFilter {
    /// Only these event types (the `type` tag, e.g. `task_moved`). `None` or an empty list for
    /// all.
    pub types: Option<Vec<String>>,
}

impl EventFilter {
    /// Only events of these types. No types means all types.
    #[must_use]
    pub fn types<I, S>(mut self, types: I) -> Self
    where
        I: IntoIterator<Item = S>,
        S: Into<String>,
    {
        let types: Vec<String> = types.into_iter().map(Into::into).collect();
        self.types = (!types.is_empty()).then_some(types);
        self
    }
}

/// How many prepared statements each connection keeps. Projections prepare their own, and
/// rusqlite's default of 16 would evict the store's.
const STATEMENT_CACHE: usize = 128;

/// The hub's SQLite store.
pub struct Store {
    /// A read-only connection for [`Store::read`], [`Store::since`] and [`Store::before`], so
    /// they never wait on the write lock (WAL readers and the writer do not block each other).
    /// `None` in network mode: `locking_mode=EXCLUSIVE` there means a second connection to the
    /// same file cannot be relied on, so reads go through `conn` instead (see [`Store::reader`]).
    ///
    /// Declared before `conn` so it closes first: fields drop in order, and only the last
    /// connection to close checkpoints the WAL and deletes `-wal` and `-shm`. A read-only
    /// connection cannot, so if it closed last the files would stay and the `.db` alone would
    /// miss recent commits.
    reader: Option<Mutex<Connection>>,
    /// The one write connection. Appends, rebuilds, `latest_rev` and the schema version go
    /// through it, and so do reads in network mode.
    conn: Mutex<Connection>,
    /// A separate lock from `conn`'s, so [`Store::register`] can add a projection through `&self`
    /// (no caller ever needs a `&mut Store`). Every method that reads or mutates this locks `conn`
    /// first and `projections` second, never the other order, so the two locks never deadlock.
    /// In practice `projections` is never contended: every writer (`append`, `rebuild`, `import`,
    /// `register`) already holds `conn` first, which alone serialises them with each other: this
    /// lock only exists to satisfy the borrow checker for mutation through a shared reference.
    projections: Mutex<Vec<Box<dyn Projection>>>,
    log_id: String,
    revs: broadcast::Sender<RevRange>,
    /// Set only in network mode: the single-host lease and its renewal thread. Checked before
    /// every write (`Error::LeaseLost` if a higher generation now exists — see
    /// [`Store::check_lease`]). Declared last, so it drops after both connections close: the
    /// lease is not released until this process is done touching the file.
    network: Option<LeaseGuard>,
}

impl std::fmt::Debug for Store {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let mut debug = f.debug_struct("Store");
        debug.field("log_id", &self.log_id);
        // `try_lock`, not `lock`: `Debug` is used from panic messages and assertions, which can
        // fire while this very thread already holds `projections` (inside `append_within`, say);
        // blocking here would deadlock instead of printing. A lock someone else holds just prints
        // as unavailable, same as any other value `Debug` cannot reach safely.
        match self.projections.try_lock() {
            Ok(guard) => {
                let names = guard.iter().map(|p| p.name()).collect::<Vec<_>>();
                debug.field("projections", &names);
            }
            Err(std::sync::TryLockError::Poisoned(e)) => {
                let guard = e.into_inner();
                let names = guard.iter().map(|p| p.name()).collect::<Vec<_>>();
                debug.field("projections", &names);
            }
            Err(std::sync::TryLockError::WouldBlock) => {
                debug.field("projections", &"<locked>");
            }
        }
        debug.finish_non_exhaustive()
    }
}

impl Store {
    /// Opens or creates the store at `path` and applies the migrations built into this binary.
    ///
    /// # Errors
    ///
    /// Database errors, a failed migration, or [`Error::SchemaTooNew`] if the database was written
    /// by a newer build.
    pub fn open(path: impl AsRef<Path>, options: StoreOptions) -> Result<Self> {
        Self::open_with(path, options, Vec::new())
    }

    /// Like [`Store::open`], with projections. Each is rebuilt if its version changed and caught
    /// up if it is behind the log, before this returns; from then on every append applies to it.
    ///
    /// # Errors
    ///
    /// As [`Store::open`], plus [`Error::DuplicateProjection`] for two projections with one name,
    /// [`Error::Projection`] if a rebuild or catch-up fails, and [`Error::ProjectionVersion`] if
    /// the store holds a projection at a higher version than this build's (a newer PitCrew
    /// rebuilt it).
    pub fn open_with(
        path: impl AsRef<Path>,
        options: StoreOptions,
        projections: Vec<Box<dyn Projection>>,
    ) -> Result<Self> {
        Self::open_with_migrations(path, options, migrations::embedded(), projections)
    }

    /// Like [`Store::open_with`], with an explicit list of migrations. For tests and tools.
    ///
    /// # Errors
    ///
    /// As [`Store::open_with`].
    pub fn open_with_migrations(
        path: impl AsRef<Path>,
        options: StoreOptions,
        migrations: &[Migration],
        projections: Vec<Box<dyn Projection>>,
    ) -> Result<Self> {
        let mut names = BTreeSet::new();
        for p in &projections {
            if !names.insert(p.name()) {
                return Err(Error::DuplicateProjection {
                    name: p.name().to_owned(),
                });
            }
        }
        let path = path.as_ref();
        let network = match options.fs {
            FsMode::Local => false,
            FsMode::Network => true,
            FsMode::Auto => {
                let dir = path
                    .parent()
                    .map_or_else(|| PathBuf::from("."), Path::to_path_buf);
                fs_kind::detect(&dir).is_network()
            }
        };

        // Network mode: take the single-host lease before touching the database at all, so two
        // hosts never both reach SQLite's own (unreliable, over a network filesystem) locking.
        let lease = network
            .then(|| LeaseGuard::acquire(path, Arc::clone(&options.clock), options.lease_ttl))
            .transpose()?;

        let flags = OpenFlags::SQLITE_OPEN_READ_WRITE
            | OpenFlags::SQLITE_OPEN_CREATE
            | OpenFlags::SQLITE_OPEN_NO_MUTEX
            | OpenFlags::SQLITE_OPEN_URI;
        let mut conn = Connection::open_with_flags(path, flags)?;
        conn.busy_timeout(options.busy_timeout)?;
        conn.set_prepared_statement_cache_capacity(STATEMENT_CACHE);
        let wanted: &'static str = if network { "delete" } else { "wal" };
        let mode = set_journal_mode(&conn, wanted, options.busy_timeout)?;
        if !mode.eq_ignore_ascii_case(wanted) {
            return Err(Error::JournalMode { wanted, got: mode });
        }
        if network {
            // SQLite's own locking is not trustworthy over a network filesystem; the lease above
            // is the real protection. This pragma is defence in depth, and the reason a second
            // connection to this file (the separate reader, below) is not opened in this mode.
            conn.pragma_update(None, "locking_mode", "EXCLUSIVE")?;
        }
        conn.pragma_update(None, "synchronous", "NORMAL")?;
        conn.pragma_update(None, "foreign_keys", "ON")?;
        migrations::apply(&mut conn, migrations)?;
        let log_id = ensure_log_id(&mut conn)?;
        for p in &projections {
            // One transaction per projection: a rebuild is all or nothing.
            let tx = conn.transaction_with_behavior(TransactionBehavior::Immediate)?;
            projection::sync(&tx, p.as_ref())?;
            tx.commit()?;
        }

        let reader = if network {
            None
        } else {
            let reader = Connection::open_with_flags(
                path,
                OpenFlags::SQLITE_OPEN_READ_ONLY
                    | OpenFlags::SQLITE_OPEN_NO_MUTEX
                    | OpenFlags::SQLITE_OPEN_URI,
            )?;
            reader.busy_timeout(options.busy_timeout)?;
            reader.set_prepared_statement_cache_capacity(STATEMENT_CACHE);
            Some(Mutex::new(reader))
        };

        let capacity = options
            .subscriber_capacity
            .clamp(1, MAX_SUBSCRIBER_CAPACITY);
        let (revs, _) = broadcast::channel(capacity);
        Ok(Self {
            reader,
            conn: Mutex::new(conn),
            projections: Mutex::new(projections),
            log_id,
            revs,
            network: lease,
        })
    }

    /// This log's id: a ULID written when the store was created, never changed after. It is the
    /// `log` in the stream's `hello` frame; revisions only compare within one log.
    #[must_use]
    pub fn log_id(&self) -> &str {
        &self.log_id
    }

    /// Resets the projection called `name` and replays the whole log into it, in one
    /// transaction. Appends wait until it is done; [`Store::read`], [`Store::since`] and
    /// [`Store::before`] do not, and see the old tables until it commits.
    ///
    /// # Errors
    ///
    /// [`Error::LeaseLost`] in network mode, if this store's lease has been taken over,
    /// [`Error::UnknownProjection`] if no projection by that name was registered,
    /// [`Error::Projection`] if it fails, [`Error::ProjectionVersion`] if the store holds it at a
    /// higher version than this build's, or database errors. Nothing changes on an error.
    pub fn rebuild(&self, name: &str) -> Result<()> {
        // `conn` locked first, `projections` second: the order every method follows (see the
        // field's doc comment), so the two locks never deadlock against `register`. The lease is
        // checked only after `conn` is actually held, not before: another writer may hold it for
        // a while (`import` can now hold it for a long import), and a check made before waiting
        // would describe a lease that may no longer be current by the time the wait ends.
        let mut conn = self.conn();
        self.check_lease()?;
        let projections = self
            .projections
            .lock()
            .unwrap_or_else(PoisonError::into_inner);
        let p = projections
            .iter()
            .find(|p| p.name() == name)
            .ok_or_else(|| Error::UnknownProjection {
                name: name.to_owned(),
            })?
            .as_ref();
        let tx = conn.transaction_with_behavior(TransactionBehavior::Immediate)?;
        if let Some(cp) = projection::checkpoint(&tx, name)?
            && cp.version > p.version()
        {
            // A newer build owns it; rebuilding with this one would downgrade it.
            return Err(projection::version_mismatch(p, cp.version));
        }
        projection::rebuild(&tx, p)?;
        tx.commit()?;
        Ok(())
    }

    /// Adds `projection` to this already-open store, without reopening it and without dropping
    /// whatever lease (network mode) or connections it already holds: catches `projection` up
    /// from its checkpoint — or runs a fresh rebuild, if it is new or its stored version is older
    /// than this build's — inside one write transaction, exactly what [`Store::open_with`] does
    /// for a projection present from the start. From the moment this returns, every
    /// [`Store::append`] and [`Store::append_new`] applies to it too.
    ///
    /// Serialises with every append, rebuild, import and registration through the one write lock
    /// this store already uses for all of those (see the `projections` field's doc comment): no
    /// event can be appended while the catch-up runs, and `projection` starts receiving events
    /// only once its tables already reflect everything appended before this call returns — so an
    /// append attempted concurrently either committed before this call started (and the catch-up
    /// already covers it) or queues behind this call's write lock and is applied to `projection`
    /// normally once it proceeds, because by then `projection` is already registered. Either way
    /// no event is missed.
    ///
    /// This is how the daemon should close the gap between its two opens (its README, "Known
    /// gaps"): call this once the work model's projections have found or added `@office`, instead
    /// of opening the store a second time with the office's run log added. In network mode that
    /// reopen drops the lease for a moment between the two opens, which another host could take;
    /// `register` never drops it, because it is still the same `Store`.
    ///
    /// **Do not call this inside [`Store::read`]'s closure in network mode**: `read` runs on the
    /// write connection there (there is no separate reader), so `register`'s own attempt to take
    /// that same connection's lock deadlocks, the same as appending would (see `read`'s doc).
    ///
    /// # Errors
    ///
    /// [`Error::LeaseLost`] in network mode, if this store's lease has been taken over,
    /// [`Error::DuplicateProjection`] if a projection by this name is already registered,
    /// [`Error::Projection`] if the catch-up fails, [`Error::ProjectionVersion`] if the store
    /// holds it at a higher version than this build's, or database errors. Nothing changes on an
    /// error: the existing projections, the log and the lease are untouched.
    pub fn register(&self, projection: Box<dyn Projection>) -> Result<()> {
        // `conn` first, lease second, same reordering and the same reason as `rebuild` and
        // `append_inner`: checked only once this call actually holds the write lock.
        let mut conn = self.conn();
        self.check_lease()?;
        if self
            .projections
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .iter()
            .any(|p| p.name() == projection.name())
        {
            return Err(Error::DuplicateProjection {
                name: projection.name().to_owned(),
            });
        }
        let tx = conn.transaction_with_behavior(TransactionBehavior::Immediate)?;
        projection::sync(&tx, projection.as_ref())?;
        tx.commit()?;
        // Still holding `conn`: no append can run between the catch-up above and this push, so
        // `projection`'s tables already reflect the log exactly as of the commit above by the
        // time anything can observe it as registered.
        self.projections
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .push(projection);
        Ok(())
    }

    /// Runs `f` on a read-only connection, inside one read transaction, so every query in it sees
    /// the same committed state. For domain crates querying their projection tables.
    ///
    /// **Local (WAL) mode:** reads use their own, separate connection: they never take the write
    /// lock, and a long read does not hold up appends (WAL readers and the writer do not block
    /// each other). Writes through it fail. Keep reads short anyway: [`Store::since`] and
    /// [`Store::before`] share that one connection and wait for the closure, and a long-open read
    /// stops the WAL from being checkpointed.
    ///
    /// **Network mode:** there is no separate connection (`locking_mode=EXCLUSIVE` cannot support
    /// one), so this runs on the write connection instead: it waits for a concurrent append (and
    /// vice versa), and the closure is not physically prevented from writing, only expected not
    /// to. See the crate README's "Network filesystems" section.
    ///
    /// The snapshot starts at the closure's first query and holds until it returns, even if
    /// events are appended meanwhile. So the revision a projection's tables reflect is its
    /// `projection_state.rev`, read inside the closure: not [`Store::latest_rev`], which is
    /// outside the snapshot, nor `MAX(events.rev)`, which may include events another process
    /// appended without this projection.
    ///
    /// **Do not call `read`, [`Store::since`] or [`Store::before`] inside the closure**: they
    /// wait for the connection the closure holds, which deadlocks. **In local (WAL) mode,
    /// appending inside the closure is fine**: `read` holds the separate reader connection's
    /// lock, a different one from the write connection `append` takes. **In network mode it
    /// deadlocks**: there is no separate reader there, so `read` already holds the *write*
    /// connection's lock for the closure's duration, and `append` (or [`Store::rebuild`] or
    /// [`Store::register`]) trying to take that same lock from the same thread waits for itself
    /// forever. `std::sync::Mutex` is not reentrant, and nothing here detects same-thread
    /// re-entry to turn it into a clean error instead.
    ///
    /// `E` is any error that a store [`Error`] converts into, including [`Error`] itself, which
    /// `?` on a [`sql::Error`](crate::sql::Error) produces.
    ///
    /// # Errors
    ///
    /// Whatever `f` returns, or a database error starting the read.
    pub fn read<T, E>(
        &self,
        f: impl FnOnce(&Connection) -> std::result::Result<T, E>,
    ) -> std::result::Result<T, E>
    where
        E: From<Error>,
    {
        let reader = self.reader();
        let tx = reader
            .unchecked_transaction()
            .map_err(|e| E::from(Error::from(e)))?;
        let out = f(&tx)?;
        // Nothing was written; ending the read transaction cannot lose anything.
        drop(tx);
        Ok(out)
    }

    /// The highest applied migration, or `None` if none are.
    ///
    /// # Errors
    ///
    /// Database errors.
    pub fn schema_version(&self) -> Result<Option<u32>> {
        migrations::current_version(&self.conn())
    }

    /// Appends events in one transaction and returns their revisions. Subscribers are told after
    /// the commit, in revision order. An empty slice appends nothing and returns an empty range.
    ///
    /// # Errors
    ///
    /// [`Error::LeaseLost`] in network mode, if this store's lease has been taken over,
    /// [`Error::DuplicateEvent`] if an event id is already stored or repeated in `events`,
    /// [`Error::Projection`] if a projection fails, [`Error::ProjectionVersion`] if another
    /// process rebuilt a projection at another version since this store opened, otherwise
    /// database or JSON errors. Nothing is appended then. To retry a batch that may be partly
    /// stored, use [`Store::append_new`].
    pub fn append(&self, events: &[Event]) -> Result<RevRange> {
        Ok(self.append_inner(events, false)?.0)
    }

    /// Like [`Store::append`], but skips events whose id is already stored (or appears earlier in
    /// `events`) instead of failing, all in one transaction. Returns the new revisions and the
    /// skipped ids, in batch order. For a runner retrying a batch after an unknown outcome.
    ///
    /// # Errors
    ///
    /// As [`Store::append`], except for [`Error::DuplicateEvent`]; nothing is appended then.
    pub fn append_new(&self, events: &[Event]) -> Result<(RevRange, Vec<EventId>)> {
        self.append_inner(events, true)
    }

    fn append_inner(&self, events: &[Event], skip_known: bool) -> Result<(RevRange, Vec<EventId>)> {
        if events.is_empty() {
            return Ok((
                RevRange {
                    from_rev: 1,
                    to_rev: 0,
                },
                Vec::new(),
            ));
        }
        let mut conn = self.conn();
        // Checked only once `conn` is actually held, not before: another writer may hold it for a
        // while (a long `import`, say), and a check made before waiting for it would describe a
        // lease that may no longer be current by the time the wait ends.
        self.check_lease()?;
        // IMMEDIATE takes the write lock up front. A deferred transaction would read first, and
        // WAL mode fails a read-to-write upgrade with SQLITE_BUSY at once, ignoring busy_timeout.
        let tx = conn.transaction_with_behavior(TransactionBehavior::Immediate)?;
        let (range, skipped) = self.append_within(&tx, events, skip_known)?;
        tx.commit()?;
        if !range.is_empty() {
            // Sent while still holding the lock, so subscribers see ranges in revision order.
            // An error only means nobody is subscribed.
            let _ = self.revs.send(range);
        }
        drop(conn);
        Ok((range, skipped))
    }

    /// The core of [`Store::append`]/[`Store::append_new`]: inserts `events` and applies them to
    /// every projection, inside the caller's own transaction `tx`. Does not commit and does not
    /// announce on `self.revs` — the caller does both, once, after this returns. Shared with
    /// [`Store::import`], which calls this once per internal batch but inside **one** transaction
    /// spanning the whole import, so a failure anywhere rolls every batch back, not just the one
    /// in progress.
    fn append_within(
        &self,
        tx: &Transaction<'_>,
        events: &[Event],
        skip_known: bool,
    ) -> Result<(RevRange, Vec<EventId>)> {
        let before = latest_rev(tx)?;
        let mut skipped = Vec::new();
        // Kept only when projections need them.
        let mut stored = Vec::new();
        // Locked once for the whole call (the is-empty check below and the apply at the end),
        // not twice: nothing else ever contends for it while this holds `tx`'s connection lock
        // (see the field's doc comment), so there is no cost to holding it a little longer, and
        // one lock is simpler than two.
        let projections = self
            .projections
            .lock()
            .unwrap_or_else(PoisonError::into_inner);
        let has_projections = !projections.is_empty();
        {
            let mut insert = tx.prepare_cached(if skip_known {
                "INSERT INTO events (id, at, workspace, author, on_behalf_of, type, data)
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7) ON CONFLICT (id) DO NOTHING"
            } else {
                "INSERT INTO events (id, at, workspace, author, on_behalf_of, type, data)
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7)"
            })?;
            for event in events {
                let (kind, data) = encode_body(&event.body)?;
                let inserted = insert
                    .execute(rusqlite::params![
                        event.id.0.to_string(),
                        event.at,
                        event.workspace.0.to_string(),
                        event.author.0.to_string(),
                        event.on_behalf_of.map(|m| m.0.to_string()),
                        kind,
                        data,
                    ])
                    .map_err(|e| duplicate_or(e, event.id))?;
                if inserted == 0 {
                    skipped.push(event.id);
                } else if has_projections {
                    stored.push(StoredEvent {
                        rev: u64::try_from(tx.last_insert_rowid()).unwrap_or(0),
                        event: event.clone(),
                    });
                }
            }
        }
        let after = latest_rev(tx)?;
        if after > before {
            apply_projections(&projections, tx, before, after, &stored)?;
        }
        Ok((
            RevRange {
                from_rev: before + 1,
                to_rev: after,
            },
            skipped,
        ))
    }

    /// Whether an event with this id is already in the log. The one supported way to ask that:
    /// `events.id` is a unique-indexed column (see the crate README, "Event log"), so this costs
    /// one indexed lookup — about the same as reading a single row of a [`Store::since`] page, not
    /// a scan of the log. Runs on [`Store::reader`]: the separate read-only connection in local
    /// mode, so it never waits on the write lock; the write connection in network mode, like
    /// [`Store::since`] and [`Store::before`]. Do not call it inside [`Store::read`] (it
    /// deadlocks, for the same reason `since` and `before` do) — the free function [`contains`]
    /// takes a connection directly, for that case: `store.read(|c| contains(c, id))`.
    ///
    /// # Errors
    ///
    /// Database errors.
    pub fn contains(&self, id: EventId) -> Result<bool> {
        contains(&self.reader(), id)
    }

    /// The newest revision, or 0 for an empty log.
    ///
    /// # Errors
    ///
    /// Database errors.
    pub fn latest_rev(&self) -> Result<u64> {
        latest_rev(&self.conn())
    }

    /// Up to `limit` events after `rev`, oldest first. `since(0, n)` starts at the beginning.
    /// Sees every append that has returned. In local (WAL) mode this runs on the separate read
    /// connection, so it does not wait for an append or a rebuild; in network mode it shares the
    /// write connection (see [`Store::read`]) and does wait. Do not call it inside
    /// [`Store::read`] (it deadlocks).
    ///
    /// # Errors
    ///
    /// Database errors, or [`Error::Corrupt`] for a row that does not decode.
    pub fn since(&self, rev: u64, limit: usize) -> Result<Vec<StoredEvent>> {
        read_events_after(&self.reader(), rev, limit)
    }

    /// Up to `limit` events matching `filter` just before `rev`, oldest first. For paging back
    /// through history: pass the first returned revision as the next `rev`. Runs on the same
    /// connection as [`Store::since`] (see its doc for the local/network difference); do not call
    /// it inside [`Store::read`].
    ///
    /// # Errors
    ///
    /// Database errors, or [`Error::Corrupt`] for a row that does not decode.
    pub fn before(&self, rev: u64, limit: usize, filter: &EventFilter) -> Result<Vec<StoredEvent>> {
        let (sql, params) = before_query(rev, limit, filter);
        let conn = self.reader();
        let mut stmt = conn.prepare_cached(&sql)?;
        let rows = stmt.query_map(rusqlite::params_from_iter(params), read_row)?;
        let mut events = rows.map(|r| r?.decode()).collect::<Result<Vec<_>>>()?;
        events.reverse();
        Ok(events)
    }

    /// A receiver of the revision ranges appended through this `Store` from now on. When it is
    /// the only writer to the file, the ranges are in order and contiguous. A receiver that falls
    /// more than `subscriber_capacity` ranges behind gets `Lagged` and should catch up with
    /// [`Store::since`].
    ///
    /// To start a stream without missing or repeating events: subscribe first, then read
    /// [`Store::latest_rev`] as `N` and send the history up to `N`, then forward received ranges,
    /// skipping any with `to_rev <= N` (they are already covered). Because `latest_rev` waits for
    /// an append in progress to commit and announce, no range straddles `N`.
    ///
    /// Only appends through this `Store` are announced, not those of another process.
    #[must_use]
    pub fn subscribe(&self) -> broadcast::Receiver<RevRange> {
        self.revs.subscribe()
    }

    fn conn(&self) -> MutexGuard<'_, Connection> {
        // A panic mid-transaction rolls the transaction back when it drops, so the connection is
        // still usable.
        self.conn.lock().unwrap_or_else(PoisonError::into_inner)
    }

    /// `Err(Error::LeaseLost)` if network mode's lease has been taken over since this `Store`
    /// opened (local mode: always `Ok`). Called by every method that writes the log or a
    /// projection's tables — [`Store::append`]/[`Store::append_new`] (via `append_inner`),
    /// [`Store::rebuild`], [`Store::register`] and [`Store::import`] (whose own batches go
    /// through `append_within` directly, each still checked, plus once more right before its
    /// final `COMMIT`) — and always *after* the write connection's lock is actually held, not
    /// before: another writer can hold that lock for a while (a long `import`, say), and a check
    /// made before waiting for it would describe a lease that may no longer be current by the
    /// time the wait ends. `LeaseGuard::check` re-lists the lease directory fresh on every call (a
    /// cheap `readdir`, no file content to read), so a takeover is caught here, immediately, not
    /// up to `ttl / RENEW_FRACTION` later when the renewal thread would next notice on its own.
    fn check_lease(&self) -> Result<()> {
        match &self.network {
            Some(guard) => guard.check(),
            None => Ok(()),
        }
    }

    /// The connection [`Store::read`], [`Store::since`] and [`Store::before`] use: the dedicated
    /// read-only connection in local mode, or (network mode has none) the write connection.
    fn reader(&self) -> MutexGuard<'_, Connection> {
        match &self.reader {
            Some(reader) => reader.lock().unwrap_or_else(PoisonError::into_inner),
            None => self.conn(),
        }
    }

    /// Copies the store to `dest` with `VACUUM INTO`: a consistent snapshot as of the moment it
    /// starts, safe to run while the store is in use elsewhere. Holds the write connection for
    /// the duration (concurrent appends through this `Store` wait; nothing is corrupted either
    /// way).
    ///
    /// # Errors
    ///
    /// [`Error::NonUtf8Path`] if `dest` is not valid UTF-8 (`VACUUM INTO` takes it as a SQL
    /// string, so a lossy conversion could silently write to the wrong path), or database
    /// errors, including SQLite refusing to overwrite a `dest` that already exists.
    pub fn snapshot(&self, dest: impl AsRef<Path>) -> Result<()> {
        let dest = dest.as_ref();
        let dest_str = dest.to_str().ok_or_else(|| Error::NonUtf8Path {
            path: dest.to_path_buf(),
        })?;
        let conn = self.conn();
        conn.execute("VACUUM INTO ?1", rusqlite::params![dest_str])?;
        Ok(())
    }

    /// Runs `PRAGMA quick_check` (or, if `full`, the slower `PRAGMA integrity_check`) on the live
    /// store. To check a file that is not open as a `Store` (for example, a snapshot), open a
    /// plain connection and call the free function [`crate::integrity_check`] directly; opening
    /// it as a `Store` would run migrations against a file that may be corrupt.
    ///
    /// # Errors
    ///
    /// Never for corruption, which [`IntegrityReport::Failed`] reports instead; a database error
    /// starting the check.
    pub fn integrity_check(&self, full: bool) -> Result<IntegrityReport> {
        maintenance::integrity_check(&self.conn(), full)
    }

    /// Writes every event in the log as JSON lines, oldest first. For tests and support, not
    /// sync: revisions are not included, since [`Store::import`] assigns fresh ones in the same
    /// order.
    ///
    /// # Errors
    ///
    /// Database errors, or [`Error::Io`] writing to `writer`.
    pub fn export(&self, mut writer: impl Write) -> Result<()> {
        const BATCH: usize = 1_000;
        let mut rev = 0;
        loop {
            let page = self.since(rev, BATCH)?;
            let got = page.len();
            for stored in page {
                let line = maintenance::encode_line(&stored.event)?;
                writeln!(writer, "{line}").map_err(Error::Io)?;
                rev = stored.rev;
            }
            if got < BATCH {
                return Ok(());
            }
        }
    }

    /// Loads `reader`'s JSON lines (as [`Store::export`] wrote them) into this store, applying
    /// every event through the same path [`Store::append`] does — so this store's registered
    /// projections build from them as usual — but all inside **one** write transaction: either
    /// every line is appended, or (a bad line, an I/O error, a lost lease) none of it is, and this
    /// store is left exactly as it was. For tests and support, not sync.
    ///
    /// **Design.** Lines are read and decoded one at a time, each capped so one hostile or
    /// corrupt line cannot force unbounded memory, and inserted in batches of 1,000, same as
    /// before; what changed is that every batch now runs inside the *same* transaction instead of
    /// its own, committed only once, at the very end — still in revision order, so the memory an
    /// import holds stays bounded by one batch (events decoded but not yet inserted) however long
    /// the file is. A fresh-file-renamed-into-place design was also considered (import into a new
    /// file, then swap it in); one transaction was chosen instead because it reuses `append`'s own
    /// insert-and-apply-projections path exactly, needs no second copy of the database on disk,
    /// and SQLite's own rollback journal already gives "nothing or everything" for free on any
    /// failure, including a real process crash mid-import (its uncommitted transaction is rolled
    /// back the next time anything opens the file, the same durability a clean `Err` return gets
    /// here). The cost, and a measurement against the old per-batch design, are in the crate
    /// README ("Maintenance" and "Timings").
    ///
    /// The store must be empty, checked from *inside* this transaction (a `latest_rev()` read
    /// before starting it could see an empty store that a concurrent writer fills a moment later,
    /// which this cannot miss: nothing else can write while this transaction holds the lock). An
    /// import is a new log, and this store's `log_id` (assigned when it was created, independently
    /// of import) already reflects that.
    ///
    /// The network-mode lease is checked after the write connection is actually held (not before:
    /// see [`Store::check_lease`]), again before every batch's insert, and once more right before
    /// the final `COMMIT` — not only between batches — so a takeover during the read of a large
    /// file, or during the last, partial batch, is still caught before anything commits.
    ///
    /// # Errors
    ///
    /// [`Error::LeaseLost`] in network mode, if this store's lease has been taken over,
    /// [`Error::NotEmpty`] if the store already holds events, [`Error::Io`] reading `reader`,
    /// [`Error::Corrupt`] decoding a line or finding one over 16 MiB, or anything else the
    /// insert-and-apply path [`Store::append`] shares can return (`DuplicateEvent`, a projection
    /// failure, a database error). **On any error, nothing is appended**: `Store::latest_rev`
    /// reads 0 afterwards, the same as before this call, because the transaction is rolled back
    /// rather than committed — never a prefix of `reader`'s lines.
    pub fn import(&self, mut reader: impl BufRead) -> Result<()> {
        const BATCH: usize = 1_000;
        let (from_rev, total) = {
            let mut conn = self.conn();
            // Checked here, once this call actually holds the write lock, not before: see
            // `check_lease`'s doc.
            self.check_lease()?;
            // One IMMEDIATE transaction for the whole import: nothing it does is visible, even to
            // this same `Store`, until `commit` below, so a `?` anywhere here (a bad line, a lost
            // lease, a projection failure) drops `tx` without committing, which rolls everything
            // back — every batch before it included.
            let tx = conn.transaction_with_behavior(TransactionBehavior::Immediate)?;
            // Read from inside `tx`, not before it starts: see this method's own doc.
            let from_rev = latest_rev(&tx)?;
            if from_rev != 0 {
                return Err(Error::NotEmpty);
            }
            let mut batch = Vec::with_capacity(BATCH);
            let mut line_no: u64 = 0;
            while let Some(line) = maintenance::read_bounded_line(&mut reader)? {
                line_no += 1;
                if line.is_empty() {
                    continue;
                }
                batch.push(maintenance::decode_line(&line, line_no)?);
                if batch.len() == BATCH {
                    self.check_lease()?;
                    self.append_within(&tx, &batch, false)?;
                    batch.clear();
                }
            }
            if !batch.is_empty() {
                self.check_lease()?;
                self.append_within(&tx, &batch, false)?;
            }
            // Once more right before the commit: a takeover while reading a slow or very large
            // `reader` (after the last batch flush, or within a batch smaller than 1,000 lines
            // entirely) must not still land as a commit.
            self.check_lease()?;
            let total = latest_rev(&tx)?;
            tx.commit()?;
            if self.reader.is_some() {
                // Local (WAL) mode: a commit appends new frames to `-wal` but does not shrink it,
                // so a large import leaves `-wal` roughly the size of what it just wrote until
                // something else happens to checkpoint it. Fold those frames back into the main
                // file and truncate `-wal` now, before releasing the lock, rather than leaving
                // that for whatever operation next happens to trigger a checkpoint. Best effort,
                // and not `?`: the commit just above already succeeded, so the import itself is
                // done and "on any error, nothing is appended" must stay true of this call — a
                // checkpoint failing here (a transient lock from a concurrent reader, say) is a
                // disk-space and performance concern, not a correctness one; the next checkpoint,
                // by any operation, catches up.
                let _ = conn.execute_batch("PRAGMA wal_checkpoint(TRUNCATE);");
            }
            (from_rev, total)
        };
        if total > from_rev {
            // Announced once, after the commit: a partial, not-yet-durable range must never reach
            // a subscriber, so this cannot happen per batch the way `append` does it. `from_rev`
            // is always 0 here today (import refuses a non-empty store above), kept rather than a
            // literal `1` so this stays correct if that restriction is ever relaxed.
            let _ = self.revs.send(RevRange {
                from_rev: from_rev + 1,
                to_rev: total,
            });
        }
        Ok(())
    }
}

/// Applies revisions `before+1..=after`, just inserted as `stored`, to every projection in
/// `projections`. A free function (not a `Store` method) so [`Store::append_within`] can call it
/// while already holding `self.projections`'s lock, without re-locking it.
fn apply_projections(
    projections: &[Box<dyn Projection>],
    tx: &Transaction<'_>,
    before: u64,
    after: u64,
    stored: &[StoredEvent],
) -> Result<()> {
    for p in projections {
        let p = p.as_ref();
        match projection::checkpoint(tx, p.name())? {
            // Another process runs another version of this projection and rebuilt it after
            // this store opened. Rebuilding it back here would replay the whole log under the
            // write lock, and the other process would do the same on its next append.
            Some(cp) if cp.version != p.version() => {
                return Err(projection::version_mismatch(p, cp.version));
            }
            Some(cp) if cp.rev == before => {
                for event in stored {
                    p.apply(tx, event)
                        .map_err(projection::failed(p, event.rev))?;
                }
                projection::set_checkpoint(
                    tx,
                    p.name(),
                    Checkpoint {
                        version: cp.version,
                        rev: after,
                    },
                )?;
            }
            // Another process appended without this projection since this store last applied
            // it: catch up from its checkpoint, the new events included.
            Some(cp) => projection::replay(tx, p, cp.rev)?,
            // Nothing deletes checkpoints; one removed by hand is rebuilt, once.
            None => projection::rebuild(tx, p)?,
        }
    }
    Ok(())
}

/// The `type` tag of an event body, e.g. `task_moved`.
///
/// # Errors
///
/// JSON errors, which the protocol types rule out.
pub fn event_type(body: &EventBody) -> Result<String> {
    Ok(encode_body(body)?.0)
}

/// Whether an event with this id is already stored, queried on `conn` directly: the free-function
/// form of [`Store::contains`], for a caller already inside [`Store::read`]'s closure (calling
/// `Store::contains` there deadlocks, the same as `since` and `before` would — see `read`'s doc).
/// [`Store::contains`] is this plus locking [`Store::reader`], for callers outside `read`.
///
/// # Errors
///
/// Database errors.
pub fn contains(conn: &Connection, id: EventId) -> Result<bool> {
    Ok(conn
        .prepare_cached("SELECT 1 FROM events WHERE id = ?1")?
        .query_row(rusqlite::params![id.0.to_string()], |_| Ok(()))
        .optional()?
        .is_some())
}

/// Up to `limit` events after `rev`, oldest first.
pub(crate) fn read_events_after(
    conn: &Connection,
    rev: u64,
    limit: usize,
) -> Result<Vec<StoredEvent>> {
    let mut stmt = conn.prepare_cached(
        "SELECT rev, id, at, workspace, author, on_behalf_of, type, data
         FROM events WHERE rev > ?1 ORDER BY rev ASC LIMIT ?2",
    )?;
    let rows = stmt.query_map(
        rusqlite::params![to_sql_rev(rev), to_limit(limit)],
        read_row,
    )?;
    rows.map(|r| r?.decode()).collect()
}

/// Writes the log id on first open. `INSERT OR IGNORE` keeps a concurrent opener's id.
fn ensure_log_id(conn: &mut Connection) -> Result<String> {
    let tx = conn.transaction_with_behavior(TransactionBehavior::Immediate)?;
    tx.execute(
        "INSERT OR IGNORE INTO meta (key, value) VALUES ('log_id', ?1)",
        [ulid::Ulid::generate().to_string()],
    )?;
    let id = tx.query_row("SELECT value FROM meta WHERE key = 'log_id'", [], |r| {
        r.get(0)
    })?;
    tx.commit()?;
    Ok(id)
}

fn latest_rev(conn: &Connection) -> Result<u64> {
    let rev: i64 = conn.query_row("SELECT COALESCE(MAX(rev), 0) FROM events", [], |row| {
        row.get(0)
    })?;
    Ok(u64::try_from(rev).unwrap_or(0))
}

/// Switches the journal mode (`WAL` for local disks, `DELETE` for network mode) and returns what
/// SQLite reports. Switching needs an exclusive lock, and when several connections open a fresh
/// file at once SQLite fails the switch with SQLITE_BUSY at once instead of calling the busy
/// handler, so retry until `timeout`. The mode is stored in the file, so later opens find it
/// already set.
fn set_journal_mode(conn: &Connection, wanted: &str, timeout: Duration) -> Result<String> {
    let deadline = std::time::Instant::now() + timeout;
    loop {
        match conn.pragma_update_and_check(None, "journal_mode", wanted, |row| row.get(0)) {
            Err(e)
                if e.sqlite_error_code() == Some(rusqlite::ErrorCode::DatabaseBusy)
                    && std::time::Instant::now() < deadline =>
            {
                // 5-10 ms, jittered so racing openers do not retry in lockstep.
                let jitter = std::time::SystemTime::now()
                    .duration_since(std::time::UNIX_EPOCH)
                    .map_or(0, |d| d.subsec_nanos() % 5_000);
                std::thread::sleep(Duration::from_micros(5_000 + u64::from(jitter)));
            }
            other => return Ok(other?),
        }
    }
}

const EVENT_COLUMNS: &str = "rev, id, at, workspace, author, on_behalf_of, type, data";

/// The SQL and parameters of [`Store::before`]. With types, one arm per type walks
/// `events_by_type_rev` backwards and `UNION ALL ... ORDER BY` merges the arms, so a page reads
/// about `limit` rows per type instead of sorting every match.
fn before_query(
    rev: u64,
    limit: usize,
    filter: &EventFilter,
) -> (String, Vec<rusqlite::types::Value>) {
    let rev = to_sql_rev(rev);
    let types: BTreeSet<&str> = filter.types.iter().flatten().map(String::as_str).collect();
    let mut params: Vec<rusqlite::types::Value> = Vec::new();
    let mut sql = if types.is_empty() {
        params.push(rev.into());
        format!("SELECT {EVENT_COLUMNS} FROM events WHERE rev < ?")
    } else {
        let arms: Vec<String> = types
            .into_iter()
            .map(|t| {
                params.push(t.to_owned().into());
                params.push(rev.into());
                format!("SELECT {EVENT_COLUMNS} FROM events WHERE type = ? AND rev < ?")
            })
            .collect();
        arms.join(" UNION ALL ")
    };
    sql.push_str(" ORDER BY rev DESC LIMIT ?");
    params.push(to_limit(limit).into());
    (sql, params)
}

/// Maps a uniqueness failure on `events.id` to [`Error::DuplicateEvent`]; any other unique
/// constraint (a later migration may add one) stays a database error.
fn duplicate_or(e: rusqlite::Error, id: EventId) -> Error {
    match &e {
        rusqlite::Error::SqliteFailure(f, Some(msg))
            if f.extended_code == rusqlite::ffi::SQLITE_CONSTRAINT_UNIQUE
                && msg == "UNIQUE constraint failed: events.id" =>
        {
            Error::DuplicateEvent { id }
        }
        _ => e.into(),
    }
}

fn to_sql_rev(rev: u64) -> i64 {
    i64::try_from(rev).unwrap_or(i64::MAX)
}

fn to_limit(limit: usize) -> i64 {
    i64::try_from(limit).unwrap_or(i64::MAX)
}

/// Splits a body into its serde tag and its content as JSON text.
fn encode_body(body: &EventBody) -> Result<(String, String)> {
    fn shape(msg: &str) -> Error {
        Error::Json(<serde_json::Error as serde::ser::Error>::custom(msg))
    }
    let mut value = serde_json::to_value(body)?;
    let obj = value
        .as_object_mut()
        .ok_or_else(|| shape("an event body serializes to a JSON object"))?;
    let kind = match obj.remove("type") {
        Some(serde_json::Value::String(s)) => s,
        _ => return Err(shape("an event body has a string `type` tag")),
    };
    let data = obj.remove("data").unwrap_or(serde_json::Value::Null);
    Ok((kind, serde_json::to_string(&data)?))
}

struct RawRow {
    rev: i64,
    id: String,
    at: i64,
    workspace: String,
    author: String,
    on_behalf_of: Option<String>,
    kind: String,
    data: String,
}

fn read_row(row: &Row<'_>) -> rusqlite::Result<RawRow> {
    Ok(RawRow {
        rev: row.get(0)?,
        id: row.get(1)?,
        at: row.get(2)?,
        workspace: row.get(3)?,
        author: row.get(4)?,
        on_behalf_of: row.get(5)?,
        kind: row.get(6)?,
        data: row.get(7)?,
    })
}

impl RawRow {
    fn decode(self) -> Result<StoredEvent> {
        let rev = u64::try_from(self.rev).unwrap_or(0);
        let corrupt = |reason: String| Error::Corrupt { rev, reason };
        let data: serde_json::Value =
            serde_json::from_str(&self.data).map_err(|e| corrupt(format!("data: {e}")))?;
        let body: EventBody = serde_json::from_value(serde_json::json!({
            "type": self.kind,
            "data": data,
        }))
        .map_err(|e| corrupt(format!("body: {e}")))?;
        let event = Event {
            id: self
                .id
                .parse::<EventId>()
                .map_err(|e| corrupt(e.to_string()))?,
            at: self.at,
            workspace: self
                .workspace
                .parse::<WorkspaceId>()
                .map_err(|e| corrupt(e.to_string()))?,
            author: self
                .author
                .parse::<MemberId>()
                .map_err(|e| corrupt(e.to_string()))?,
            on_behalf_of: self
                .on_behalf_of
                .map(|m| m.parse::<MemberId>())
                .transpose()
                .map_err(|e| corrupt(e.to_string()))?,
            body,
        };
        Ok(StoredEvent { rev, event })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn pragmas_after_open() {
        let dir = tempfile::tempdir().expect("tempdir");
        let store =
            Store::open(dir.path().join("store.db"), StoreOptions::default()).expect("open");
        let conn = store.conn();
        let pragma = |name: &str| -> String {
            conn.query_row(&format!("PRAGMA {name}"), [], |r| {
                r.get::<_, rusqlite::types::Value>(0)
            })
            .map(|v| match v {
                rusqlite::types::Value::Integer(i) => i.to_string(),
                rusqlite::types::Value::Text(s) => s,
                other => format!("{other:?}"),
            })
            .expect("pragma")
        };
        assert_eq!(pragma("journal_mode"), "wal");
        // 1 is NORMAL.
        assert_eq!(pragma("synchronous"), "1");
        assert_eq!(pragma("foreign_keys"), "1");
    }

    fn plan(store: &Store, filter: &EventFilter) -> String {
        let (sql, params) = before_query(100, 10, filter);
        let conn = store.conn();
        let mut stmt = conn
            .prepare(&format!("EXPLAIN QUERY PLAN {sql}"))
            .expect("prepare");
        let rows = stmt
            .query_map(rusqlite::params_from_iter(params), |r| {
                r.get::<_, String>(3)
            })
            .expect("query")
            .collect::<rusqlite::Result<Vec<_>>>()
            .expect("rows");
        rows.join("\n")
    }

    #[test]
    fn filtered_before_walks_the_index_without_sorting() {
        let dir = tempfile::tempdir().expect("tempdir");
        let store =
            Store::open(dir.path().join("store.db"), StoreOptions::default()).expect("open");
        for filter in [
            EventFilter::default().types(["task_moved"]),
            EventFilter::default().types(["task_moved", "file_edited"]),
            EventFilter::default().types(["task_moved", "file_edited", "ask_raised"]),
        ] {
            let plan = plan(&store, &filter);
            assert!(plan.contains("events_by_type_rev"), "{plan}");
            assert!(!plan.contains("TEMP B-TREE"), "{plan}");
        }
        let plan = plan(&store, &EventFilter::default());
        assert!(!plan.contains("TEMP B-TREE"), "{plan}");
    }

    #[test]
    fn other_unique_failures_are_not_duplicate_events() {
        let dir = tempfile::tempdir().expect("tempdir");
        let store =
            Store::open(dir.path().join("store.db"), StoreOptions::default()).expect("open");
        let conn = store.conn();
        conn.execute_batch("CREATE TABLE u (x INTEGER UNIQUE) STRICT; INSERT INTO u VALUES (1);")
            .expect("setup");
        let e = conn
            .execute("INSERT INTO u VALUES (1)", [])
            .expect_err("unique");
        assert!(matches!(
            duplicate_or(e, EventId::new()),
            Error::Database(_)
        ));
    }

    #[test]
    fn subscriber_capacity_is_clamped() {
        let dir = tempfile::tempdir().expect("tempdir");
        let options = StoreOptions {
            subscriber_capacity: usize::MAX,
            ..StoreOptions::default()
        };
        // tokio panics above usize::MAX / 2; the clamp keeps this from reaching it.
        let store = Store::open(dir.path().join("store.db"), options).expect("open");
        drop(store.subscribe());
    }

    #[test]
    fn rev_range_len_saturates() {
        let r = RevRange {
            from_rev: 5,
            to_rev: u64::MAX,
        };
        assert_eq!(r.len(), u64::MAX - 4);
        let all = RevRange {
            from_rev: 0,
            to_rev: u64::MAX,
        };
        assert_eq!(all.len(), u64::MAX);
    }
}
