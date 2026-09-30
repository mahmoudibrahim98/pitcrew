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

With other agents building on the same machine, worst pages reached about 10–13 ms, for `main`'s
code as well; the targets hold on an idle machine.