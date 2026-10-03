# Brief 0 · Fix five more flaky tests

- **Stream:** 0 · Contracts (test fixes in two streams). **Branch:** `integrator/flaky-tests-2`.
  **Paths:** `crates/api/tests/terminal.rs`, `crates/runner/tests/fake_source.rs`, `crates/runner/tests/hooks.rs` and `crates/hub-work/tests/recap_file.rs`.
- **First read:** [README.md](README.md), the root `AGENTS.md` (or `CLAUDE.md`), and
  [0-flaky-tests.md](0-flaky-tests.md) (PR #19), whose rules apply here too: no retries, no skipped
  assertions, and a looser bound only with a reason.
- **Suggested agent:** Codex, or any coding agent.

## Goal

Two tests failed on CI runners for timing reasons unrelated to the PRs they ran on (#14 and #16 on
2026-10-03). Make each one prove its point without depending on how fast a shared runner is.

## What to fix

1. **`a_stalled_write_holds_up_neither_output_nor_pings`** (`crates/api/tests/terminal.rs`
   ~528-566; failed on macOS).
   - **What it proves:** while a terminal write is stalled, output still flows: more frames than
     the queue holds (`queue_frames: 2`), for longer than the pump would wait on a full queue
     (`send_timeout: 300 ms`), and before the stalled call is given up (`call_timeout: 1.5 s`).
   - **Why it's flaky:** it counts rounds in a fixed 700 ms window and asserts `rounds >= 5`. A slow
     runner fits fewer rounds.
   - **Fix:** loop until both conditions hold: at least 5 rounds (more than the queue), and more
     than `send_timeout` plus a margin elapsed since the stall began. Bound the loop safely below
     `call_timeout`, and assert that both conditions were reached before that bound.
2. **`new_items_arrive_fast_and_a_restart_resumes_from_the_cursor`**
   (`crates/runner/tests/fake_source.rs` ~45-75; failed on Windows).
   - **What it proves:** each new item, announced by a write, becomes an event quickly, and a
     restart resumes from the cursor without re-reading.
   - **Why it's flaky:** `latencies.iter().all(|l| *l < 300ms)`. One slow file notification on a
     loaded Windows runner fails it.
   - **Fix:** keep the latency check, but make it robust and say why. For example: the median under
     300 ms (the product's budget) and every one under a generous ceiling (say 1.5 s) that still
     catches a missed notification, which would fall back to the slow schedule.
   - The budget itself is measured properly in `benches/`. Don't touch the restart half of the
     test.

3. **`a_stalled_runtime_answers_503_or_closes_with_1011`** (`crates/api/tests/terminal.rs`; failed natively on Windows while a parallel build ran, then passed 5/5 alone).
   - **The problem:** it got a 503 during the WebSocket handshake, where it expected the stream to open and then close with 1011.
   - **The question:** is either answer correct under load (its name allows both)? If so, make the test accept a 503 at the handshake as well as 1011 after the open. If not, find the timing that lets the stall reach the handshake.
4. **The `recap_file.rs` property test** (`crates/hub-work/tests/recap_file.rs`; found reviewing PR #30) fails whenever it draws an empty `specs`. Decide whether an empty input is a valid case. If it is, make the property hold for it; if not, make the strategy never generate it. Say which.
5. **`crates/runner/tests/hooks.rs` ~944** (an assertion `first < second`; failed on ubuntu CI for PR #32, passed 3/3 locally). Find the timing it depends on, and make it deterministic in the same way: assert the ordering it means, not wall-clock luck.

## Acceptance

- Each test passes 30 runs in a row in the environment, under load, e.g. with a parallel
  `cargo build` running. Report the loop.
- Each still fails when the behaviour breaks. Show it once: make the stalled write block output
  (for 1), or disable the watcher so only the slow schedule picks items up (for 2).
- fmt, clippy with `-D warnings`, the guards, and every CI job pass.

## Out of scope

Other tests, and product changes.
