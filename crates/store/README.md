# pitcrew-store

SQLite store: migrations, the append-only event log, projections, and NFS-safe mode.

**Owned by stream C** — see [docs/build/streams/C.md](../../docs/build/streams/C.md).

## Migrations

`build.rs` embeds every `migrations/NNNN_<name>.sql` file (four digits, a lowercase name), sorted
by number. Number ranges belong to streams (ADR-0004), so a stream adds its file under its own
range and this crate picks it up with no code change. A misnamed SQL file (including a wrongly
cased `.SQL`) or a repeated number fails the build. Other files, such as notes in Markdown, are
ignored.

`Store::open` records applied versions in `schema_migrations`, applies every known migration that
is missing, and refuses a database with a version newer than the binary knows. Each migration runs
in its own IMMEDIATE transaction that re-checks `schema_migrations` first, so two processes opening
a fresh file at once both succeed and each migration runs once.

### Rules for writing a migration

- **No transaction or connection control.** No `BEGIN`, `COMMIT`, `ROLLBACK` or `SAVEPOINT` (the
  store wraps each file in a transaction), no `VACUUM`, and no `journal_mode` or `synchronous`
  pragmas (the store sets them on open).
- **Tables are `STRICT`.**
- **Foreign keys are off while a migration runs.** The store turns them off before the
  transaction (the pragma does nothing inside one), runs `PRAGMA foreign_key_check` before
  commit, fails the migration if any reference dangles, and turns them back on afterwards. So a
  table rebuild (create new, copy, drop old, rename) is safe: dropping the old table does not
  cascade to or null its children. Do not set `foreign_keys` or `defer_foreign_keys` yourself.
- **A merged migration is never edited.** Fix it with a new migration.
- Use your stream's number range (listed in `0001_init.sql`).

## Event log

- `append(&[Event]) -> RevRange` writes in one IMMEDIATE transaction; revisions are gap-free from
  1. If any id is already stored or repeated, it fails with `Error::DuplicateEvent` and stores
  **nothing** from the batch, not even its new events.
- `append_new(&[Event]) -> (RevRange, skipped ids)` stores only the events whose ids are not
  already present, in one transaction. Use it to retry a batch after an unknown outcome.
- `since(rev, limit)` pages forward; `before(rev, limit, &EventFilter)` pages back. An empty type
  filter matches everything. With types, `before` merges one `(type, rev)` index walk per type,
  so a page costs about `limit` rows per type however large the log is.
- `subscribe()` is a `tokio::sync::broadcast` receiver of new revision ranges. When this `Store`
  is the only writer to the file, ranges arrive in order and contiguous; appends by another
  process are not announced. A receiver that falls behind gets `Lagged` and catches up with
  `since`.

To start a stream (the hello frame) without gaps or repeats: subscribe, then read
`latest_rev()` as `N`, send history up to `N`, then forward received ranges, skipping those with
`to_rev <= N`.

`log_id()` is a ULID written to `meta` when the store is created (or on the first open of an
older store) and never changed. It is the `log` in the hello frame: revisions only compare within
one log.

## Projections

Tables derived from the log (pages, the Inbox, recaps) are **projections**. A domain crate:

1. creates its tables in its own migration range (`02xx` for E, and so on);
2. implements `pitcrew_store::Projection`: `name()` (stable, unique, e.g. `work.tasks`),
   `version()`, `reset(&Transaction)` (clear its tables) and `apply(&Transaction, &StoredEvent)`;
3. passes it to `Store::open_with(path, options, vec![Box::new(…)])`.

Then:

- every `append` applies its events to every projection **in the same transaction**. If `apply`
  returns an error, the whole append rolls back: nothing is stored, no revision is used, nothing
  is announced. An error is a bug, never skipped;
- `projection_state (name, version, rev)` records each projection's progress. On open, a
  projection whose stored `version` is older (or that is new) is rebuilt: `reset`, then the log
  replayed 1,000 events at a time. One that is behind the log (another process appended without
  it) catches up. Each rebuild or catch-up is one transaction, so readers never see half a
  rebuild. A stored version **newer** than this build's is refused with
  `Error::ProjectionVersion`, like a newer schema: rebuilding it would undo the newer build;
- an append only catches up on revisions, for a projection another process appended past without
  it. If another process rebuilt a projection at a **different version** after this store
  opened, the append fails with `Error::ProjectionVersion` and stores nothing. Rebuilding it
  back instead would replay the whole log under the write lock on every append, in both
  processes. Two builds with different projection versions cannot share a store;
- `Store::rebuild(name)` rebuilds one on demand (and also refuses a newer stored version);
- bump `version()` whenever `apply` changes meaning. `reset` followed by replaying the log must
  give the same tables as applying events one append at a time; test that.

**SQL access.** `pitcrew_store::sql` re-exports the store's `rusqlite`, so domain crates use the
workspace's one version and the same `Transaction` type without their own dependency. `apply`
and `reset` get the write transaction: do not commit, and touch only your own tables. A
`DbError` wraps a `sql::Error`; `DbError::as_sql()` reaches it (e.g. for its error code).

**Reads.** `Store::read(|conn| …)` runs a closure on a separate **read-only** connection inside
one read transaction (a consistent snapshot from the closure's first query until it returns,
even if appends commit meanwhile). Reads never take the write lock, so a slow read does not hold
up appends and an append does not block reads (WAL). `since` and `before` use the same
connection, so they do not wait for an append or a `Store::rebuild` either; `latest_rev` stays on
the writer. Keep reads short: they share one connection, and a long-open read stops the WAL from
being checkpointed. The closure's error type is anything a store `Error` converts into, e.g.
`pitcrew_store::Result<T>`, where `?` on a `sql::Error` works.

**Do not call `read`, `since` or `before` inside a `read` closure: it deadlocks** (they wait for
the connection the closure holds). Appending inside one is fine.

**Closing.** The read connection closes before the write connection, so the writer is the last
to close and checkpoints the WAL: no `-wal` or `-shm` file is left, and the `.db` alone holds
every commit.

### Rules for projections

For every domain crate (E, F, G) that writes one:

- **`apply` has no side effects.** It writes only its own tables in the transaction it is given:
  no appending events, no files, network or channels, and no reading the clock. It must give the
  same tables when the log is replayed a year later; time comes from `event.at`.
- **`apply` never reads other projections' tables.** Their state at that moment depends on
  registration order and on their own rebuilds. Join across projections in `Store::read`.
- **No foreign keys between different projections' tables.** One projection's `reset` must not
  cascade into, or be blocked by, another's rows.
- **Who acted comes from the event:** `event.author` and `event.on_behalf_of` of the
  `StoredEvent`, never the caller of the current request (a replay has no caller).
- **A migration that reshapes a projection's tables ships with a `version()` bump,** so the next
  open rebuilds the tables from the log instead of keeping rows in the old shape.
- **Inside `read`, the revision the data reflects is `projection_state.rev`** for that
  projection, queried in the same closure. Not `MAX(events.rev)` (another process may have
  appended events not yet applied) and not `Store::latest_rev()` (outside the snapshot).
- **Network mode** (`journal_mode=DELETE` plus EXCLUSIVE locking) has no concurrent readers:
  `read`, `since` and `before` route through the write connection instead. See the next section.

## Network filesystems and the single-host lease

HPC home directories are often NFS, Lustre or GPFS. SQLite's WAL mode needs shared memory, which a
network filesystem does not provide safely, and SQLite's own file locking is not trustworthy over
one either (that is the whole reason this mode exists). `StoreOptions.fs` chooses how `Store::open`
decides:

- **`FsMode::Auto`** (the default) calls `pitcrew_store::detect` on the database's directory.
  **Linux** reads `statfs`'s `f_type` magic number; **macOS** reads its `f_fstypename`; **Windows**
  treats a UNC path (`\\server\share`, `\\?\UNC\...`) as network and a drive letter as local (a
  mapped network drive looks local to `std`; force `FsMode::Network` for those). An unrecognised
  type (`FsKind::Unknown`) is treated the same as `FsKind::Network`: guessing "local" wrongly is
  the unsafe direction, so anything not on the allowlist (`ext2`/`3`/`4`, `xfs`, `btrfs`, `zfs`,
  `tmpfs`, `f2fs`, `bcachefs`, `overlay(fs)`, `apfs`, `hfs` and similar) takes the slower, safe
  path.
- **`FsMode::Local`** and **`FsMode::Network`** force the choice, for callers who know better and
  for tests.

**Local mode** is unchanged: WAL, no lease. **Network mode**:

- `journal_mode=DELETE`, `locking_mode=EXCLUSIVE`, the same `synchronous=NORMAL`.
- No separate read-only connection: `locking_mode=EXCLUSIVE` means a second connection to the file
  cannot be relied on, so `Store::read`, `Store::since` and `Store::before` run on the write
  connection. They therefore wait for a concurrent append (and vice versa); this is the documented
  cost of network mode, not a bug.
- **The lease.** A lease file next to the database, `<db>.lease` (e.g. `store.db.lease`), holds
  JSON:
  `{"host": "...", "pid": ..., "owner": "<ulid>", "until_ms": <epoch ms>}`. `Store::open` takes it
  atomically (temp file plus rename in the same directory; mode 0600 on Unix, directory ACL on
  Windows) or fails with `Error::Leased { host, pid, until }`. It is taken over once `until_ms` has
  passed, or — same host only — once the pid it names is no longer running (checked with
  `kill(pid, 0)` through `rustix`, no `unsafe`; not possible on Windows, so there it is expiry
  only). A torn or garbage file (one that does not parse as that JSON) is treated as expired only
  once its mtime is older than `lease_ttl`: a lease mid-write is not mistaken for a free one.
  - **Renewal is automatic**, from a small thread `Store::open` starts, not a method callers must
    remember to call: it wakes every `lease_ttl / 3` (so one slow or missed wakeup still leaves
    two tries before the lease would actually expire), checks the file still names this `Store`'s
    `owner`, and only then writes a fresh `until_ms`. If the owner ever does not match — another
    host took over because this process's clock stalled or it was suspended — the thread stops and
    every later `append` fails with `Error::LeaseLost`, never silently past a lease this store no
    longer holds.
  - **`StoreOptions.clock`** (a `Clock`, defaulting to `SystemClock`) is where the lease gets the
    time; inject one in tests to expire a lease without sleeping.
  - Dropping the `Store` stops the thread and releases the lease (deletes the file), but only if
    it still names this store's `owner` — if someone else already took over, there is nothing of
    ours left to release.

## Maintenance

- **`Store::snapshot(dest)`** copies the store with `VACUUM INTO`: a consistent copy as of the
  moment it starts, safe to call while the store is in use (appends through this `Store` wait for
  the duration; nothing is corrupted either way). `dest` must not already exist.
- **`Store::integrity_check(full)`** runs `PRAGMA quick_check` (or, if `full`,
  `PRAGMA integrity_check`) and returns `IntegrityReport::Ok` or `Failed(messages)` — never an
  `Err` for corruption itself. The free function `pitcrew_store::integrity_check(conn, full)`
  checks any plain connection (e.g. to a snapshot or a raw copy) without opening it as a `Store`,
  which would run migrations against a file that may be corrupt.
- **`Store::export(writer)`** writes every event as one JSON line each (no revision: order is the
  record), oldest first. **`Store::import(reader)`** loads those lines into an empty store
  (`Error::NotEmpty` otherwise), appending them through the normal `append` path so this store's
  registered projections build from them. An import is a new log: `log_id` was already assigned
  when the store was created, independently of import, so it differs from the exported store's.
  Both are for tests and support, not sync.

## Timings

`cargo test -p pitcrew-store --release --test perf -- --ignored --nocapture`. Targets: append
under 1 s, each page under 5 ms.

Measured 2026-09-30 on a laptop (Intel Core Ultra 5 135U, 14 threads, 16 GB), WSL2 Ubuntu
22.04, release build, six runs on an otherwise idle machine:

| Operation | Mean per run | Worst page |
|---|---|---|
| Append 10,000 events in batches of 100 | 208–304 ms total | — |
| `since(rev, 100)`, 100 pages | 0.31–0.36 ms | 1.39 ms |
| `before(rev, 100)`, one type (1 in 5 events), 21 pages | 0.28–0.36 ms | 1.01 ms |
| `before(rev, 100)`, two types, 27 pages | 0.29–0.36 ms | 0.94 ms |
| Append 10,000 events in batches of 100, two projections | 345–396 ms total (without: 310–340 ms, same session) | — |
| Rebuild one projection over 10,000 events | 45–76 ms | — |

With other agents building on the same machine, worst pages reached about 10–13 ms, for `main`'s
code as well; the targets hold on an idle machine.