# Brief C · Import all-or-nothing; adding projections without dropping the lease

- **Stream:** C · Store. **Branch:** `s/C/import-and-reopen`. **Paths:** `crates/store/**`
  (+ `Cargo.lock`).
- **First read:** [README.md](README.md), the `crates/store` README (export and import, leases,
  projections), `docs/security/threat-model.md` (finding R7), `fuzz/regressions/store_import/`, and
  `crates/daemon/README.md` ("Known gaps": the lease gap between two opens).

## Goal

Three fixes found by the fuzzers and the daemon review. Each makes the store safer to use without
changing what it stores.

## What to build

1. **Import is all or nothing** (finding R7).
   - Today `Store::import` commits 1,000-line batches. An export whose 1,001st line is bad leaves
     1,000 events behind, and a second import is then refused (`NotEmpty`).
   - Make a failed import leave the store exactly as it was: one transaction, or a staging table,
     or a fresh file renamed into place. Pick one, measure it, and say why.
   - Imports of large logs must stay bounded in memory.
   - **Tests:**
     - Q's reproduction (`fuzz/regressions/store_import/`);
     - a bad line at 1, at 1,000, at 1,001 and at the end;
     - a crash mid-import, simulated by dropping the store in the middle, then reopening: either
       nothing was imported, or the import is resumable or refused cleanly;
     - `latest_rev() == 0` after every failure.
2. **`Store::contains(EventId) -> Result<bool>`.** Other crates query `events.id` directly today.
   Give them a supported call, documented with its cost.
3. **Projections after open, without dropping the lease.**
   - The daemon opens the store twice: once with the work model's projections, to find or add the
     back office's member, then again with the office's run log. Between the two, the lease is
     released, and on a network filesystem another host could take it.
   - Provide a way to add a projection to an open store while keeping its lease. For example,
     `Store::register(projection)` catches the projection up from its checkpoint before it starts
     receiving appends, or an equivalent you prefer.
   - Test that the lease generation doesn't change, and that the projection sees every event,
     including ones appended during its catch-up.
   - Document how the daemon should switch to it. The daemon is stream 0; don't edit it.

## Acceptance

- The tests above pass. The fuzz target `store_import` passes on Q's inputs
  (`PITCREW_FUZZ_SKIP_KNOWN` is no longer needed for R7).
- No behaviour change for successful imports or normal opens: the existing tests are unchanged.
- fmt, clippy (Linux and Windows target), `cargo test --workspace`, `cargo deny check`, and the
  guards all pass.

## Out of scope

The daemon's switch-over (stream 0), and NFS caching beyond what is already documented.
