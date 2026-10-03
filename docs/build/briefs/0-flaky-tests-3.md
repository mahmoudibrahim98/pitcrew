# Brief 0 · Two more flaky tests

- **Stream:** 0 · Contracts (cross-stream test fixes).
  **Branch:** `integrator/flaky-tests-3`.
  **Paths:** `crates/runner/tests/fake_source.rs`, `crates/daemon/tests/scan.rs`, and the code under
  test only if the race is in the product, not the test (say which).
- **First read:**
  - [README.md](README.md) and the root `AGENTS.md` (or `CLAUDE.md`);
  - [0-flaky-tests-2.md](0-flaky-tests-2.md) and PR #34, for how the last round kept each test's
    failure check.
- **Suggested agent:** Codex, on Windows if possible (one of the two fails there).

## What to fix

Keep what each test checks. Make it wait for the condition, not for a guess at a time.

1. **`stop_is_prompt_during_a_long_backfill`** (`crates/runner/tests/fake_source.rs` ~605).
   - **Seen:** on Windows CI (#38's run), "stop during backfill took 4.3 s after 2 of 100 reads".
   - **Fix:** find whether the stop waits on a read already in flight, or on the scheduler of a slow
     runner. If the product's stop really can take seconds, that's a bug: fix it. Otherwise give the
     test a bound that still fails if the stop waits for the backfill to finish.
2. **`a_scan_streams_progress_and_reports_the_fixtures`** (`crates/daemon/tests/scan.rs` ~392).
   - **Seen:** on Linux CI (#39's run), "no scan in the log". The test reads the daemon's log right
     after the scan's last frame, and the summary line can land a moment later.
   - **Fix:** wait (with a deadline) for the summary line, or log it before the final frame is sent,
     if that's the better order for the product. Keep the checks that the line holds the counts and
     no path.

## Acceptance

- Each test passes 30 runs in a row under load (for example with `cargo test` running in parallel),
  and still fails when its fault is injected (say how).
- fmt, clippy with `-D warnings`, the tests of the crates touched, and the guards pass. Every CI job
  passes on the pull request.
