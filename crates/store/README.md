# pitcrew-store

SQLite store: migrations, the append-only event log, projections, and NFS-safe mode.

**Owned by stream C** — see [docs/build/streams/C.md](../../docs/build/streams/C.md).

## Migrations

`build.rs` embeds every `migrations/NNNN_<name>.sql` file (four digits, a lowercase name), sorted
by number. Number ranges belong to streams (ADR-0004), so a stream adds its file under its own
range and this crate picks it up with no code change. A misnamed SQL file or a repeated number
fails the build. Other files, such as notes in Markdown, are ignored.

`Store::open` records applied versions in `schema_migrations`, applies every known migration that
is missing (each in its own transaction), and refuses a database with a version newer than the
binary knows.

## Event log

- `append(&[Event]) -> RevRange` writes in one transaction; revisions are gap-free from 1.
- `since(rev, limit)` pages forward; `before(rev, limit, &EventFilter)` pages back.
- `subscribe()` is a `tokio::sync::broadcast` receiver of new revision ranges. A receiver that
  falls behind gets `Lagged` and catches up with `since`.

Timings (`cargo test -p pitcrew-store --release --test perf -- --ignored --nocapture`): see the
test for the targets.
