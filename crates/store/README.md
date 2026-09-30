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
- **Foreign keys are on** during migrations, and `PRAGMA foreign_keys` cannot be changed inside a
  transaction. To rebuild a table (create new, copy, drop old, rename), start the file with
  `PRAGMA defer_foreign_keys = ON;` so the checks run at commit.
- **A merged migration is never edited.** Fix it with a new migration.
- Use your stream's number range (listed in `0001_init.sql`).

## Event log

- `append(&[Event]) -> RevRange` writes in one IMMEDIATE transaction; revisions are gap-free from
  1. A repeated event id fails the whole batch with `Error::DuplicateEvent`.
- `since(rev, limit)` pages forward; `before(rev, limit, &EventFilter)` pages back. An empty type
  filter matches everything.
- `subscribe()` is a `tokio::sync::broadcast` receiver of new revision ranges, delivered in order
  and contiguous. A receiver that falls behind gets `Lagged` and catches up with `since`.

To start a stream (the hello frame) without gaps or repeats: subscribe, then read
`latest_rev()` as `N`, send history up to `N`, then forward received ranges, skipping those with
`to_rev <= N`.

## Timings

`cargo test -p pitcrew-store --release --test perf -- --ignored --nocapture`; see the test for the
targets.
