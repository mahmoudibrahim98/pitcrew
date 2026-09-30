# Brief C · Open, migrate, and the event log

- **Stream:** C · Store. **Branch:** `s/C/open-and-log`. **Paths:** `crates/store/*`,
  `crates/store/src/**`, `crates/store/tests/**`, `crates/store/benches/**`,
  `crates/store/migrations/01*`. **Do not edit** `crates/store/migrations/0001_init.sql`
  (stream 0).
- **First read:** [README.md](README.md), then `docs/build/streams/C.md`, ADR-0004,
  `crates/store/migrations/0001_init.sql`, `crates/protocol/src/events.rs`.

## Goal

The hub store's foundation: open a SQLite database, apply **every stream's migrations** in order,
and read and write the **append-only event log** with revisions. This is work packages 1 and 2 of
your card.

## What to build

1. **`Store::open(path, StoreOptions)`** with `rusqlite` (`workspace = true`, bundled):
   - WAL on local disks for now (NFS mode is a later brief);
   - `foreign_keys=ON`, a busy timeout, and `synchronous=NORMAL` under WAL.
2. **Migrations:**
   - A `build.rs` embeds **every** file in `migrations/` named `NNNN_<name>.sql`, sorted by
     number, with `cargo:rerun-if-changed=migrations`. Other streams' files (02xx, 03xx, …) are
     then picked up without editing this crate.
   - Reject duplicate numbers and bad names at build time.
   - Track applied versions in a table the code creates itself before running migrations, e.g.
     `schema_migrations(version, name, applied_at)`.
   - Each migration runs in its own transaction.
   - Opening a database with a version newer than the binary knows fails with a clear error.
3. **Event log API:**
   - `append(&[Event]) -> RevRange`: one transaction. Store the id as the ULID string, the
     `EventBody` serde tag in `type`, and its content in `data` as JSON, exactly the columns of
     `0001_init.sql`.
   - `latest_rev()`, `since(rev, limit)`, and `before(rev, limit, filter)`. The filter is an
     optional list of event types for now; its shape should be able to grow to
     project/workstream/task/session later.
   - Reading reconstructs the exact `Event` (round trip).
   - `subscribe()` returns a receiver of new revision ranges, for the API's delta stream. Choose
     between a bounded `tokio::sync::broadcast` and a std channel, and say why in your report.
4. Errors are a `thiserror` enum. The API never exposes rusqlite types.

## Acceptance

- The fixture's 15 events (`pitcrew_fixtures::demo_workspace()`) append and read back equal,
  with revisions 1–15.
- `UPDATE` and `DELETE` on `events` fail (the triggers from `0001`).
- Opening twice applies nothing new; a database with a newer version is refused.
- A migration added under `migrations/` in a test fixture directory is picked up (test the
  embedding logic separately from `build.rs` if that is simpler).
- 10,000 events appended in batches of 100 in under 1 s on SSD, and `since(rev, 100)` in under
  5 ms (a criterion bench, or an ignored test that prints timings; report the numbers).

## Out of scope

Projections (next brief), NFS detection and leases, domain tables, HTTP. No `unsafe`.
