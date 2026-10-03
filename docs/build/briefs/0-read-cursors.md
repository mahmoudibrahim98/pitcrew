# Brief 0 · "Since you last looked": per-person read cursors

- **Stream:** 0 · Contracts (contract, work model, recap and the projects UI together).
  **Branch:** `integrator/read-cursors`.
  **Paths:**
  - `docs/build/contracts/api-v1.md`;
  - `crates/protocol/**` (types and events; regenerate `packages/protocol-ts`);
  - `crates/hub-work/**` and its migrations `crates/store/migrations/02*`;
  - `crates/recap/**`, only if recaps need the cursor;
  - `apps/ui/src/projects/**` and `apps/ui/src/data/**`;
  - `apps/mock-hub/**`, `tests/conformance/**`;
  - the READMEs.
- **First read:**
  - [README.md](README.md) and the root `AGENTS.md` (or `CLAUDE.md`);
  - `docs/build/streams/F.md` (item 6, per-member read cursors) and `N.md` (item 1, Home: "what changed since you last looked");
  - `api-v1.md` (recaps, activity, the delta stream);
  - `crates/hub-work/README.md` and `apps/ui/src/projects/README.md`.
- **Suggested agent:** Codex, or any coding agent.

## Goal

Home should tell a person what changed **since they last looked**, not just recently. That needs
a read cursor per person: the log revision they had seen, per scope, kept on the hub, so it follows
them across devices.

## What to build

1. **The contract first:**
   - a cursor per person, and optionally per scope (`workspace`, or a project or workstream):
     `GET /v1/me/cursors` and `PUT /v1/me/cursors/{scope}` with `{rev}`;
   - **person-only:** an agent token gets 403;
   - a cursor only moves forward; a `PUT` with an older rev is a no-op that returns the current one;
   - whatever the activity and recap routes need to answer "since my cursor" (for example a
     `since=cursor` shorthand). Keep it small, and write it into `api-v1.md`.
2. **Hub:**
   - storage through events and a projection (a `cursor_moved` event, or a table written by a
     command, following hub-work's patterns: say which and why);
   - the routes;
   - tests: forward-only, per person, per scope, 403 for agents.
3. **The UI:**
   - Home's "what changed" section uses the cursor: items newer than it are marked new, with a
     count;
   - "Mark all as read" moves the cursor to the newest revision shown;
   - looking at a project or workstream page moves that scope's cursor once the person has been on
     it for a moment, not on every render;
   - live updates through the stream, as today;
   - unit tests, and an end-to-end test against the mock hub, with axe in both themes.
4. **The mock hub and conformance:**
   - the mock hub implements the routes;
   - conformance covers them against both targets (forward-only, 403 for an agent, per person).

## Acceptance

- fmt, clippy with `-D warnings`, the tests of the crates touched, the UI's checks (typecheck, lint,
  test, build, e2e), the protocol-ts freshness check, the conformance suite against both targets, and
  the guards pass. Every CI job passes on the pull request.

## Out of scope

Notifications, unread counts in the desktop tray (a later brief), and per-task cursors.
