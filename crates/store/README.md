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
  projection whose `version` changed (or that is new) is rebuilt: `reset`, then the log replayed
  1,000 events at a time. One that is behind the log (another process appended without it)
  catches up. Each rebuild or catch-up is one transaction, so readers never see half a rebuild.
  An append also catches up a projection that fell behind while the store was open;
- `Store::rebuild(name)` rebuilds one on demand;
- bump `version()` whenever `apply` changes meaning. `reset` followed by replaying the log must
  give the same tables as applying events one append at a time; test that.

**SQL access.** `pitcrew_store::sql` re-exports the store's `rusqlite`, so domain crates use the
workspace's one version and the same `Transaction` type without their own dependency. `apply`
and `reset` get the write transaction: do not commit, and touch only your own tables.

**Reads.** `Store::read(|conn| …)` runs a closure on a separate **read-only** connection inside
one read transaction (a consistent snapshot). Reads never take the write lock, so a slow read
does not hold up appends and an append does not block reads (WAL). Keep reads short: they share
one connection, and a long-open read stops the WAL from being checkpointed. The closure's error
type is anything a store `Error` converts into, e.g. `pitcrew_store::Result<T>`, where `?` on a
`sql::Error` works.

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