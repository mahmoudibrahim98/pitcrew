# Brief 0 · Dispatch follow-up: a CLI that exits while the daemon is down

- **Stream:** 0 · Composition root (runner and work model).
  **Branch:** `integrator/dispatch-followups-2`.
  **Paths:** `crates/runner/**`, `crates/hub-work/**`, `crates/daemon/**`, the READMEs of the crates
  touched, and `docs/build/contracts/api-v1.md` only to keep it true.
- **First read:**
  - [README.md](README.md) and the root `AGENTS.md` (or `CLAUDE.md`);
  - [0-dispatch-followups.md](0-dispatch-followups.md) and PR #39 (merged), with its review comments;
  - `crates/runner/README.md` (terminals, commands).
- **Suggested agent:** Codex, or any coding agent.

## What to fix

- **The problem** (Codex's review of #39): `crates/runner/src/commands.rs` ~159-160. If a dispatched
  CLI exits before the daemon restarts, `RunnerTerminals::new()` calls `refresh()`, which deletes the
  terminal rows the runtime no longer lists. The dispatch's indexed transcript then looks like an
  imported session, so the reconciliation hears `Reported` forever instead of ending the dispatch,
  and the agent can't be dispatched again.
- **Fix:** keep the evidence across the refresh: that the runner started this terminal for a
  dispatch, and that it is gone (a tombstone, or the provenance kept on the session). After a
  restart, a dispatched session whose terminal is gone ends its dispatch, as it does without a
  restart. An imported session (never started by the runner) stays as it is.
- **Tests,** failing before the fix: dispatch, let the CLI exit while the daemon is stopped, restart;
  the dispatch ends and the agent can be dispatched again. An imported transcript with no terminal is
  not ended.

## Acceptance

- fmt, clippy with `-D warnings`, `cargo test -p pitcrew-runner -p pitcrew-hub-work -p pitcrew-daemon`,
  `npm test`, the conformance suite against both targets, and the guards pass. Every CI job passes on
  the pull request.
