# Brief 0 · The recap contract: wire types, routes and the mock

- **Stream:** 0 · Contracts (integration work). **Branch:** `integrator/recap-contract`.
  **Paths:**
  - `crates/protocol/**`;
  - `crates/recap/**` (moving its wire types only, no behaviour change);
  - `docs/build/contracts/api-v1.md`;
  - `apps/mock-hub/**`;
  - `crates/fixtures/**`;
  - plus `Cargo.lock`.
- **First read:** [README.md](README.md), `crates/recap/README.md` and its `block.rs`,
  `summary.rs` and `day.rs`, `docs/build/streams/F.md` and `N.md`, `docs/build/contracts/api-v1.md`
  (paging and activity), `apps/mock-hub/README.md`, and ADR-0007.

## Goal

The recap engine (stream F) builds activity blocks and day paragraphs whose every clause cites
receipts, but nothing serves them. Define how the API serves recaps, put the wire types in the
protocol, and make the mock hub serve the demo workspace's real recaps, so the UI (stream N) and
the API (stream H) can build against them in parallel.

## What to build

1. **Wire types in `pitcrew-protocol`** (a new `recap` module):
   - Move the serialisable types the API will send out of `crates/recap`: `Block`, `BlockKey`,
     `Counts`, `FileTouch`, `Fact`, `FactKind`, `Summary`, `Span`, `DayRecap`, and the date type.
   - `crates/recap` re-exports them from the protocol, so its code and tests don't change and its
     snapshots stay byte-identical. If a type carries behaviour that can't move, keep the
     behaviour in recap as a free function or an extension trait.
   - Check that `Span`'s range serialises as `{ "start", "end" }`, in **UTF-8 byte offsets on
     character boundaries**, and say so in the contract. The UI must convert them to string
     indices.
   - Add the page types: `RecapBlock` (a `Block` plus its one-line `Summary`), `BlocksPage`, and
     `DaysPage`.
2. **Routes** in `api-v1.md`, a new "Recaps" section (device tokens; agents too, if reads stay open
   to agents as elsewhere):
   - `GET /v1/recaps/blocks?session=&task=&workstream=&project=&before=<block id>&limit=`:
     - newest first, each block with its line;
     - filters as in the activity route, with the same link-following rules;
     - `before` is exclusive; `limit` defaults to 50, at most 200;
     - answers `{ blocks, at_start }`.
   - `GET /v1/recaps/days?workstream=|project=&tz=<minutes east of UTC>&before=<YYYY-MM-DD>&limit=`:
     - day paragraphs, newest day first, at most 30 days;
     - for a project, one entry per workstream per day, plus its tasks outside any workstream;
     - `tz` decides where days begin;
     - answers `{ days, at_start }`.
   - **Semantics to write down:**
     - recaps are derived, never stored as events, so the server may cache them by the event
       range they cover;
     - a block still open (its last event within the gap) may still grow;
     - every span's receipts point at events or items the caller can read;
     - text is the engine's cleaned text, which is still untrusted, so render it as text.
   - **Errors:** `400 invalid` for a malformed id, date or `tz`; an unknown id gives an empty page,
     as in the activity route.
   - **Live updates:** say how the UI learns that recaps changed. Proposal: no new event; refetch
     when the activity it covers changes, with keys the data layer can invalidate on events in
     scope. Write the rule for the data layer.
3. **The mock hub** serves both routes from a fixture:
   - Add a test in `crates/fixtures` (or recap's dev tests) that writes
     `crates/fixtures/data/demo-recaps.json` from the demo workspace with the `RuleSummarizer`:
     every block with its line, and the day paragraphs for `tz=0`.
   - Check that file in. A test fails when it is stale (regenerate with an env var, as other
     snapshots do).
   - The mock serves pages from it with the same filters, paging and errors. For `tz`, it serves
     only `tz=0` and answers `400 invalid` otherwise; document that.
   - Mock tests.
4. **The parity replay** (`crates/daemon/parity`) is out of your paths. Write the rows it should add
   (the two routes, with the demo's filters) into your report instead.

## Acceptance

- `cargo test --workspace`: recap's snapshots are unchanged, and the protocol round-trips the new
  types (serde tests).
- The mock's tests cover both routes: filters, paging, `before`, `tz`, and errors.
- `api-v1.md` documents the routes, types, semantics and the live-update rule.
- fmt, clippy (Linux and Windows target), `cargo deny check`, `npm test`, and the guards all pass.

## Out of scope

The real routes (stream H), computing recaps in the daemon, read cursors ("since you last
looked", F's work package 6), the UI (stream N), and the TypeScript types in `src/data` (stream
L).
