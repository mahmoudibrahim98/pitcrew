# Stream C · Store

**Goal:** the hub's SQLite store: migrations, the append-only event log with revisions,
projection machinery, and safe operation on network filesystems.

**Owns:** `crates/store/` (manifest, README, `src`, `tests`, `benches`),
`crates/store/migrations/01*`.  **Depends on:** stream 0.  **Model:** Sonnet-class.
**Read first:** ADR-0004; `crates/store/migrations/0001_init.sql`;
`crates/protocol/src/events.rs`.

## Work packages

1. **Open and migrate.** `Store::open(path, options)` with `rusqlite` (bundled). A `build.rs`
   embeds **every** `migrations/NNNN_*.sql` file in order, so other streams' migrations (02xx E,
   03xx F, 04xx G, 05xx O) are picked up without editing this crate. Record applied versions;
   refuse a store whose schema is newer than the binary.
2. **Event log.** `append(events) -> rev range` in one transaction; `since(rev, limit)`;
   `before(rev, limit, filter)`; subscribe to new revisions (a broadcast channel) for the delta
   stream. Events are stored as in `0001_init.sql`.
3. **Projection machinery.** A `Projection` trait (`apply(&Tx, &Event)`, `reset(&Tx)`) with a
   registry; projections run in the same transaction as the append; a `rebuild()` replays the
   log. Domain projections themselves belong to E, F and G.
4. **Network filesystems.** Detect NFS, CIFS, Lustre, GPFS and similar (`statfs` on Linux and
   macOS). Prefer a node-local state directory; otherwise `journal_mode=DELETE`,
   `locking_mode=EXCLUSIVE`, and a **single-host lease** (lease file with host, pid, expiry;
   take over only after expiry). Local disks use WAL.
5. **Maintenance.** Online backup, `VACUUM INTO` snapshots, integrity check, and a store-level
   `export`/`import` for tests.

## Acceptance

- Migrations are idempotent; a newer schema is refused with a clear error.
- Updates and deletes on `events` fail (triggers).
- Rebuilding projections from the log gives the same tables as incremental application
  (tested with the fixture events and a generated 100k-event log).
- Appending 10,000 events in batches of 100 takes < 1 s on SSD; `since(rev)` pages in < 5 ms.
- NFS mode is exercised in tests by forcing the detection result.

## Do not

Define domain tables (E, F, G own theirs), expose HTTP routes, or edit `0001_init.sql` (stream 0).
