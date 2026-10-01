# Brief 0 · `pitcrewd` serves recaps; one open of the store

- **Stream:** 0 · Composition root (integration work). **Branch:** `integrator/daemon-recaps`.
  **Paths:** `crates/daemon/**` (+ `Cargo.lock`).
- **First read:** [README.md](README.md), [0-daemon-wiring.md](0-daemon-wiring.md) (merged), the
  `crates/daemon` README ("Known gaps"), the `crates/api` README ("Recaps": `RecapSource`,
  `Recaps::new(..).routes()`), the `crates/hub-work` README ("Recaps": `RecapIndex`,
  `sync_recaps`, the adapter notes), the `crates/store` README (`Store::register`), and
  `docs/build/contracts/api-v1.md` ("Recaps").

## Goal

The real daemon answers `GET /v1/recaps/blocks` and `GET /v1/recaps/days` from the hub's recap
index. It opens its store once, so the network lease is never dropped.

## What to build

1. **The recap routes:**
   - **An adapter** from `pitcrew_hub_work::RecapIndex` (implemented by `WorkService`) to
     `pitcrew_api::RecapSource`, the same shape as `src/refs.rs`:
     - copy the four filter fields;
     - map `DaysScope` variant by variant;
     - pass `Some(limit)` and `before.as_ref()`;
     - map errors.
     - An `invalid` from the index after the route has validated is a bug: log it at warn.
   - **Mount** `Recaps::new(adapter).routes()` as a device route.
   - **Warm-up:** after seeding (`--demo`) and before serving, call `work.sync_recaps()` once on
     the blocking pool. Log the revision and the elapsed time, or warn on failure. Don't block
     start-up on it: requests still work while it runs, because the index catches up on query.
2. **One open of the store:**
   - Replace the drop and reopen around the back office's member with
     `store.register(Box::new(back_office.run_log()))` (the store crate's README has the snippet).
   - Remove the "lease between the two opens" entry from "Known gaps".
   - The office tests must still pass unchanged.
   - Add a test that the lease generation is the same before and after the office starts
     (network mode, if the test setup supports it; otherwise explain why the code path proves
     it).
3. **Tests** (`crates/daemon/tests`):
   - with `--demo`:
     - both routes answer 200 with well-formed pages;
     - blocks are newest first;
     - every span's UTF-8 slice is non-empty, with at least one receipt;
     - a workstream's days equal its entries among its project's days;
     - paging to `at_start` gives the whole list;
     - `?tz=60` works (the daemon supports any `tz`; the mock doesn't);
   - 403 for an agent token and 401 without one;
   - after an API write (a comment on a task), the next blocks query reflects it.
4. **The parity replay** (`parity/replay.mjs`): add invariant rows for recaps. The seeded demo
   can't equal the mock's fixture (E's README says why), so compare properties rather than
   values:
   - **Blocks:**
     - `limit=200` gives 200, `at_start`, ids strictly descending, and every line has spans;
     - paging by 4 concatenates to the same list, with no empty page before the start;
     - `before=<the oldest id>` gives an empty page at the start;
     - filters (`session=SES0002`, `task=TSK0001`, `workstream=WST0002`, `project=PRJ0002`):
       every block carries the link;
     - `project=PRJ0002&session=SES0002` gives an empty page at the start;
     - an unknown task id gives an empty page at the start;
     - the prefixed lower-case session id equals the bare one;
     - `session=nope`, `task=PAP-1`, `before=1790761920000` and `limit=0` give 400;
     - an agent token gives 403, and no token gives 401.
   - **Days:**
     - `project=PRJ0001` gives dates descending, the entry without a workstream first and then
       by workstream id;
     - a workstream's days deep-equal the project's entries for it;
     - each day's `blocks` ids are in the blocks list with the same workstream;
     - `workstream=WST0004` gives an empty page at the start;
     - paging by `limit=1` gives one date per page, concatenating to the whole;
     - `tz=0` equals no `tz`;
     - `tz=841`, `tz=1.5` and `tz=UTC` give 400;
     - no scope, both scopes, `before=2026-13-01`, `before=2026-9-30` and `limit=0` give 400;
     - an agent token gives 403.
   - **Known difference:** `tz=60` gives 400 on the mock and 200 on the daemon.
   - Run the replay against both servers, and report the remaining differences, each with its
     reason.

## Acceptance

- The routes match the contract on the real daemon, and the tests above pass.
- The parity replay shows only expected differences.
- The UI e2e (the shell suite) against the daemon still passes.
- fmt, clippy (Linux and Windows target), `cargo test --workspace`, `cargo deny check`, and the
  guards all pass.

## Out of scope

Persisting recap blocks (a later E brief), read cursors, and `pitcrewd connect` (after J's tunnel).
