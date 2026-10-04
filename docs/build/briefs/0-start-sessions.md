# Brief 0 · Start a session from the app, and stale "working" states

- **Stream:** 0 · Composition root (console UI, runner state).
  **Branch:** `integrator/start-sessions`.
  **Paths:** `apps/ui/src/console/**`, `apps/ui/src/projects/**` (a "Start session" entry on a
  workstream), `apps/ui/src/shell/**` (only to register the "+ New → Session" entry),
  `apps/ui/src/data/**`, `crates/runner/**`, `crates/ingest/**`, `crates/daemon/**`,
  `crates/protocol/**` (regenerate `packages/protocol-ts`), `docs/build/contracts/api-v1.md`,
  `apps/mock-hub/**`, `tests/conformance/**`, and the READMEs of what you touch. Mechanical edits
  elsewhere are fine; say which in the report.
- **First read:**
  - [README.md](README.md) and the root `AGENTS.md` (or `CLAUDE.md`);
  - `api-v1.md`: `POST /v1/sessions` (`StartSession`), sessions, machines, terminals;
  - `apps/ui/src/shell/README.md` (how a feature registers a `create` entry; its example is a
    disabled "Session" entry) and `apps/ui/src/console/`;
  - [0-create-dialogs.md](0-create-dialogs.md): in progress in parallel, and it touches "+ New".
    Register your entry from the console feature, and don't edit the shell's `CREATE` list.
- **Suggested agent:** Codex.

## Goal

A smoke test of the app against a real daemon found two gaps a person hits in their first
minutes.

1. There is **no way to start an agent session from the app**. The hub has `POST /v1/sessions`,
   but no button calls it.
2. **Old sessions stay "Working" forever.** A Codex session last written days ago still shows
   Working in the console, on Home and in the sidebar count.

## What to build

1. **New session:** "+ New → Session", a button in the console's session list, and "Start session"
   on a workstream (prefilled with its location). The dialog asks for:
   - engine: Claude, Codex or OpenCode, only those available on the machine;
   - machine and folder: a workstream's location, or a path validated for that machine's platform;
   - permission mode: default, plan or accept edits; bypass only where the runner allows it;
   - optional first prompt and title.
   It then calls `POST /v1/sessions` and opens the new session in the workbench, with its terminal.
   Errors (`503` no runner, `409`, CLI not found) are shown inline, with what to do.
2. **Stale states:** a session stays Working, Waiting or Starting only while something says it's
   alive: recent transcript writes, hooks, or a live terminal or process that PitCrew knows about.
   - Without any of these for a bounded time (choose it and justify it; minutes, not days), it is
     shown as Idle.
   - It's derived on read or set by the runner; nothing is rewritten in the transcript.
   - A later write makes it Working again.
   - Restarting the daemon must not mark every old session Working.
3. **Tests:** the dialog against the real hub and the mock hub; a fake CLI starting end to end;
   stale-state rules (a fresh write, an old write, a hook, a live terminal, a restart); conformance
   for whatever the API exposes.

## Acceptance

- Starting a session from each of the three entry points works against the real hub on Windows,
  macOS and Linux (CI), and the session appears with a terminal.
- After a restart, a home of old transcripts shows no Working sessions.
- fmt, clippy with `-D warnings`, the tests of the crates touched, `cargo test -p pitcrew-protocol
  --features ts`, `npm test`, the UI's checks, both conformance targets, and the guards pass. Every
  CI job passes on the pull request.
