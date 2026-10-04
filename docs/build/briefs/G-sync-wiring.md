# Brief G · GitHub and Jira, wired in: setup, credentials, read sync into the hub

- **Stream:** G · Integrations (with the daemon, the desktop and a settings UI).
  **Branch:** `integrator/sync-wiring`.
  **Paths:** `crates/sync-github/**`, `crates/sync-jira/**`, `crates/store/migrations/04*`;
  `crates/daemon/**`, `crates/hub-work/**` (applying sync intents through its commands);
  `crates/protocol/**` (regenerate `packages/protocol-ts`); `apps/ui/src/**` (an Integrations
  settings page and the workstream's external links); `apps/desktop/src-tauri/src/**` only for
  credential hand-off; `docs/build/contracts/api-v1.md`; `apps/mock-hub/**`, `tests/conformance/**`;
  the threat model; the READMEs of what you touch.
- **First read:**
  - [README.md](README.md) and the root `AGENTS.md` (or `CLAUDE.md`);
  - `docs/build/streams/G.md` (all of it), ADR-0006 and ADR-0007 (the `sync` mover);
  - [G-github-read.md](G-github-read.md), [G-jira-read.md](G-jira-read.md),
    [G-fuzz-findings.md](G-fuzz-findings.md), and both crates' READMEs ("Known gaps").
- **Suggested agent:** an Opus-class agent.

## Goal

The read-only GitHub and Jira sync crates exist but nothing runs them. Wire them in: a person
connects GitHub (repos) or Jira (projects) to a workspace, links a workstream to a repo/milestone
or a Jira epic, and the hub keeps tasks and workstreams in step with upstream — **reading only**.
Outward writes are the next brief (G-approval-writes).

## What to build

1. **Contract:** routes for integration setup and status (G.6): add/remove a connection, test it,
   link/unlink a workstream to an upstream scope, read the last sync time, issues and errors.
   Device tokens only.
2. **Credentials (G.5):** from `gh auth token` on the primary machine or entered once in the desktop
   (handed to the daemon by the gateway, never through the webview); stored owner-only, never logged,
   never returned by any route; repo-scoped where possible.
3. **The sync loop in the daemon:** incremental, rate-limit aware (ETags / JQL cursors as the crates
   already do), on a timer and on demand; applies the crates' intents through hub-work's commands
   with the `sync` mover rules (an upstream close moves a task to done only when `can_move(Sync)`
   allows; in-progress work is never touched); conflicts become asks. Close the "Known gaps" for
   milestones/epics → workstreams.
4. **UI:** an Integrations settings page (connect, test, status, errors) and, on a workstream, its
   external links and last sync.
5. **Tests:** recorded HTTP fixtures only (no network in CI); conformance for the routes on both
   targets (the mock hub implements them over fixtures).

## Acceptance

- fmt, clippy with `-D warnings`, the tests of the crates touched, `cargo test -p pitcrew-protocol
  --features ts`, `npm test`, the UI's checks, both conformance targets, and the guards pass. Every
  CI job passes on the pull request.
- A threat-model row for integration credentials and upstream data (untrusted input).

## Out of scope

Any outward write (G-approval-writes), webhooks.
