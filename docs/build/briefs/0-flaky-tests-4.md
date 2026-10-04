# Brief 0 · Two more flaky tests

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

## Acceptance

- Each test passes 30 runs in a row under load and still fails when its fault is injected (say
  how).
- The UI's checks, the tests of the crates touched, and the guards pass. Every CI job passes on the
  pull request.
