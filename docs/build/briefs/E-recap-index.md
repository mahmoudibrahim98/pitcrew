# Brief E · The recap index: blocks and day recaps, kept current in the hub

- **Stream:** E · Work model. **Branch:** `s/E/recap-index`. **Paths:** `crates/hub-work/**`
  (+ `Cargo.lock`).
- **First read:** [README.md](README.md), `docs/build/contracts/api-v1.md` ("Recaps"), the
  `crates/recap` README (`blocks`, `BlockBuilder`, `Directory`, `day_recaps`, `block_line`),
  `crates/protocol/src/recap.rs`, and `crates/hub-work`'s README ("Wiring", the activity index
  and `EventRefs`, which this mirrors).

## Goal

The hub can answer the two recap routes from what it already knows, cheaply and correctly: blocks
of work with their lines, and day paragraphs, all with receipts. Stream H builds the routes over a
seam in parallel. The daemon then connects the two with a small adapter, as it did for the activity
index.

## What to build

1. **A recap index** in `hub-work`:
   - It keeps the recap engine's `BlockBuilder` fed with every appended event, in log order, and a
     `Directory` seeded from the work model's projections and kept current from events.
   - On open it rebuilds from the log. Measure how long that takes for 100,000 events (release),
     and report it.
   - Keep it consistent with the single-writer rules: no event is missed between the build and
     the live updates (the same subscribe-then-read pattern as the back office).
2. **Queries,** exactly as the contract says:
   - `recap_blocks(filter, before, limit) → BlocksPage`:
     - newest first by block id;
     - filters on the block's links (session, any of its tasks, workstream, project), combined;
     - `before` exclusive, with limit defaults and caps from the protocol constants;
     - a page that is not at the start is never empty;
     - each block with its `block_line`.
   - `recap_days(scope, tz_minutes, before, limit) → DaysPage`:
     - `day_recaps` with the `RuleSummarizer` over the blocks in scope (a workstream, or a
       project's workstreams plus its work outside any workstream);
     - newest date first; within a date, the entry without a workstream first, then by
       workstream id;
     - pages hold whole dates.
   - **Cache** day recaps by scope, `tz` and the blocks they cover, so a repeated query doesn't
     recompute. A block that grows invalidates only the days it touches. Keep the cache bounded.
   - Validation of `tz` and limits is the API's job, but don't panic on any input.
3. **Tests:**
   - Against the demo fixture, the index's blocks and days equal `crates/fixtures/data/demo-recaps.json`
     when fed the same events. If the seeded demo differs, explain exactly why, and test equality
     on the fixture's events.
   - Paging and filters; a growing open block; incremental updates equal a rebuild (a property
     test, as recap's own `incremental.rs`); the cache's bound; and that every receipt points at an
     event in the log.

## Acceptance

- The queries match the contract, and the tests above pass.
- The rebuild time for 100,000 events in release is reported.
- fmt, clippy (Linux and Windows target), `cargo test --workspace`, `cargo deny check`, and the
  guards all pass.

## Out of scope

The routes (stream H), the daemon's adapter (stream 0), the UI, and read cursors.
