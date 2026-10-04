# Brief 0 · Three more flaky tests

- **Stream:** 0 · Contracts (cross-stream test fixes).
  **Branch:** `integrator/flaky-tests-4`.
  **Paths:** `apps/ui/src/projects/tests/task-drawer.test.tsx` and `crates/runner/tests/**`, plus
  the code under test only if the race is in the product, not the test (say which).
- **First read:**
  - [README.md](README.md) and the root `AGENTS.md` (or `CLAUDE.md`);
  - [0-flaky-tests-3.md](0-flaky-tests-3.md) and PR #42, for how each fix kept its failure check.
- **Suggested agent:** Codex, or any coding agent.

## What to fix

Keep what each test checks. Make it wait for the condition, not for a guess at a time.

1. **`TaskDrawer > lets a person tick their own subtasks and add one`**
   (`apps/ui/src/projects/tests/task-drawer.test.tsx`).
   - **Seen:** in #40's UI job (Linux CI), 1 of 720 tests failed; a re-run passed. The test likely
     asserts before an optimistic update or a refetch has settled.
   - **Fix:** await the state the test means (`findBy…`, `waitFor` on the condition), and check the
     component doesn't leave a pending update that races the next action.
2. **`forced_polling_sees_appends`** (`crates/runner/tests/`).
   - **Seen:** on Windows CI (#42's first run), alongside a slow-runner latency failure; a re-run
     passed.
   - **Fix:** find what it waits on (a polling interval, a fixed sleep) and wait on the observed
     append instead, with a deadline that still fails if polling never sees it.

3. **`a_ptyd_that_closes_during_hello_counts_as_none`** (`crates/ptyd/tests/protocol.rs` ~312).
   - **Seen:** on macOS CI (#44's run): `no ptyd, no terminals: Unavailable("cannot write to
     pitcrew-ptyd: Socket is not connected (os error 57)")`. On macOS, a peer that closes during the
     hello can surface as ENOTCONN on the write rather than as a clean close.
   - **Fix:** decide whether that error should also count as "no ptyd" (the product code, if so;
     the path list grows by `crates/runtime/**` or `crates/ptyd/**` where the hello is handled), or
     make the test's stand-in close at a point where the outcome is deterministic. Say which.

## Acceptance

- Each test passes 30 runs in a row under load and still fails when its fault is injected (say
  how).
- The UI's checks, the tests of the crates touched, and the guards pass. Every CI job passes on the
  pull request.
