# Brief C · Projections and the log id

- **Stream:** C · Store. **Branch:** `s/C/projections`. **Paths:** `crates/store/*`,
  `crates/store/src/**`, `crates/store/tests/**`, `crates/store/benches/**`,
  `crates/store/migrations/01*`.
- **First read:** [README.md](README.md), then `docs/build/streams/C.md` (work package 3),
  ADR-0004, and the current `crates/store` on `main`, including its README's migration rules.

## Goal

The machinery every domain stream (E work model, F recap, G sync) builds its tables on:
**projections** that stay in step with the event log. Also a stable **log id** for the API's
stream `hello`.

## What to build

1. **Log id.** At store creation, write a new ULID to `meta` (`log_id`); it never changes after.
   Add `Store::log_id() -> &str` (read once at open). An existing store without one gets one on
   first open by this version. This is the `log` in the stream's `hello` frame; see
   `StreamFrame::Hello` and `api-v1.md`.
2. **`Projection` trait.** Roughly:
   - `name()`, and `version()` (bump it to force a rebuild);
   - `reset(&Tx)`, which clears the projection's tables;
   - `apply(&Tx, &StoredEvent)`.

   Domain crates write SQL, so decide how they reach the transaction. The recommended way is to
   re-export rusqlite as `pitcrew_store::sql`, so everyone uses the workspace's version. Document
   the choice.
3. **Registry.** Projections are passed when opening, e.g. `Store::open_with(path, options,
   projections)`. They then run **inside the same transaction as `append`**. A projection error
   rolls back the whole append and returns an error, because it is a bug and must not be
   silently skipped.
4. **Checkpoints.** A `projection_state(name, version, rev)` table, in a new `01xx` migration. On
   open, each projection whose `version` changed is rebuilt from the log (`reset` + replay), and
   each one behind `latest_rev` catches up. Replay in batches so a large log doesn't load into
   memory. Add `Store::rebuild(name)`.
5. **Reads.** Domain crates need to query their tables. Provide `Store::read(|conn| …)` with a
   read-only view (or a separate read connection), and document that reads never take the write
   lock for long.

## Acceptance

- A toy projection in the tests (e.g. events counted by type and by author) gives the same tables
  whether applied incrementally or rebuilt from scratch, over the fixture events and a generated
  log of 50,000 events.
- Bumping `version` triggers a rebuild on the next open; a projection behind `latest_rev` catches
  up; a failing `apply` rolls back the append (nothing stored, no rev used).
- `log_id` is stable across reopen and differs between two new stores.
- Timing: appending 10,000 events with two projections registered (report the number).

## Out of scope

Domain tables and projections (E, F, G own them), NFS mode and leases (a later brief).
