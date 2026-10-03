# Brief 0 · Dispatch follow-ups: three lifecycle edge cases (#33's re-review)

- **Stream:** 0 · Composition root (runner, work model and daemon together).
  **Branch:** `integrator/dispatch-followups`.
  **Paths:**
  - `crates/runner/**`, `crates/hub-work/**`, `crates/daemon/**`;
  - `crates/protocol/**` only if an event or command must change (regenerate `packages/protocol-ts`);
  - `docs/build/contracts/api-v1.md` and `docs/security/threat-model.md`, only to keep them true;
  - the READMEs of the crates touched.
- **First read:**
  - [README.md](README.md) and the root `AGENTS.md` (or `CLAUDE.md`);
  - [0-dispatch.md](0-dispatch.md) (adoption ~40-45, the task moving ~62-64, ending ~68-69);
  - PR #33's body and the integrator's review comments on it;
  - `crates/runner/README.md`, `crates/hub-work/README.md` (dispatch).
- **Suggested agent:** Codex, or any coding agent.

## What to fix

Codex's review of #33 (merged) found three cases where a dispatch loses track of its session.
For each one, write the test first and check that it fails on `main`.

1. **A transcript written before its CLI exits is still adopted.**
   - **The problem:** `crates/runner/src/watch.rs` ~2209-2212. When Codex or OpenCode writes its
     transcript and exits before the watcher reads it, `has_ended` rules its terminal out. The
     transcript then gets a new session id without the dispatch's agent and task, and the
     reconciliation fails the dispatch.
   - **Fix:** a process that has exited may still have written the right transcript. Before retiring
     the terminal, scan for and match transcripts that already exist.
2. **A first read that already holds a finished turn still moves the task.**
   - **The problem:** `crates/hub-work/src/dispatch.rs` ~481-490. If the runner's first read of a
     transcript holds both work and a completed turn, it folds the early state changes
     (`emit_states: !first`) and reports the session as idle. `dispatch_working` is never called, so
     the task stays in todo or backlog, and the agent's later review report gets 409 (that move
     needs in progress).
   - **Fix:** keep the first "working" change for a named (dispatched) session, or treat equivalent
     evidence of work as enough here. Say which and why.
3. **A dispatched CLI that exits ends its dispatch.**
   - **The problem:** `crates/hub-work/src/query.rs` ~704-706. Once a dispatched session has its
     native id, it leaves the reconciliation for good. If its CLI then exits with no review report
     and no SessionEnd hook (Codex quits or crashes; Codex's notify only says idle), nothing sees the
     terminal die. The dispatch stays active and blocks a new dispatch to that agent.
   - **Fix:** keep watching the terminals of active dispatches that have a transcript too, and finish
     the dispatch when its terminal exits.

## Acceptance

- fmt, clippy with `-D warnings`, the tests of the crates touched, `npm test`, the conformance suite
  against both targets, and the guards pass. Every CI job passes on the pull request.
- The report names, for each item, the test that failed before the fix.

## Out of scope

Session linking by folder or branch (brief 0-session-linking), and new dispatch features.
