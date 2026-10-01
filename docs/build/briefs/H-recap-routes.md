# Brief H · The recap routes

- **Stream:** H · API and auth. **Branch:** `s/H/recap-routes`. **Paths:** `crates/api/**`
  (+ `Cargo.lock`).
- **First read:** [README.md](README.md), `docs/build/contracts/api-v1.md` ("Recaps", and
  "Asks, briefs, activity" for the paging conventions), `crates/protocol/src/recap.rs` (the page
  types and limit constants), and the `crates/api` README (the `EventRefs` seam, which this
  mirrors).

## Goal

`GET /v1/recaps/blocks` and `GET /v1/recaps/days` exactly as the contract says, over a seam the
daemon fills in. Stream E builds the hub's recap index in parallel.

## What to build

1. **A `RecapSource` seam** in `pitcrew-api`, like `EventRefs`:
   - `blocks(filter, before, limit) → BlocksPage`;
   - `days(scope, tz_minutes, before, limit) → DaysPage`;
   - its own filter and scope types (the daemon maps them, as it does for `RefFilter`);
   - errors through the existing source error.
   - It is mounted with a builder, e.g. `Recaps::new(source).routes()`. Without a source, the
     routes aren't mounted.
2. **The routes:**
   - Device tokens only; agent tokens get 403.
   - **Validation**, everything that is `400 invalid`:
     - malformed ids (bare or prefixed), dates (`YYYY-MM-DD` only) and limits (0, non-numeric);
     - `tz` out of -840..=840, or not a whole number;
     - days with neither `workstream` nor `project`, or with both.
   - A limit above the cap counts as the cap.
   - An unknown id gives an empty page with `at_start: true`.
   - Call the source on the blocking pool, as the activity route does.
3. **Tests,** with a fake source:
   - every validation case;
   - auth (401 and 403);
   - that parameters reach the source correctly (filters combined, `before`, the limit capped);
   - the response bodies, which are exactly the protocol types.
   - Then run the mock hub's recap test cases against your routes with a fake source fed from
     `crates/fixtures/data/demo-recaps.json`, and report any difference from the mock.

## Acceptance

- The routes match the contract, and the tests above pass.
- `--no-default-features` still builds (CI checks it).
- fmt, clippy (Linux and Windows target), `cargo test --workspace`, `cargo deny check`, and the
  guards all pass.

## Out of scope

Computing recaps (stream E), the daemon's adapter (stream 0), the UI, and read cursors.
