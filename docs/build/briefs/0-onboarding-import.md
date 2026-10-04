# Brief 0 · Onboarding: import sessions on a real hub

- **Stream:** 0 · Contracts (contract, daemon, runner, onboarding UI and mock together).
  **Branch:** `integrator/onboarding-import`.
  **Paths:**
  - `docs/build/contracts/api-v1.md`, `crates/protocol/**` (regenerate `packages/protocol-ts`);
  - `crates/daemon/**`, `crates/runner/**` (only what the import filter needs), `crates/hub-work/**`
    if the filter is hub state;
  - `crates/api/**`, for one visibility check shared with read-cursor privacy (#36's
    `stream.rs` and `activity.rs` already hide events while keeping revisions true);
  - `apps/ui/src/onboarding/**`, `apps/mock-hub/**`, `tests/conformance/**`;
  - the READMEs of what you touch.
- **First read:**
  - [README.md](README.md) and the root `AGENTS.md` (or `CLAUDE.md`);
  - `docs/build/streams/O.md` (item 1: "import sessions (all, by filter, or start fresh; read in
    place, reversible)") and `docs/adr/0010-real-cli-sessions.md` ("Import is read in place");
  - [0-onboarding-scan.md](0-onboarding-scan.md) and `api-v1.md` "Machine scan";
  - `apps/ui/src/onboarding/api.ts` (`ImportFilter`, `ImportDryRunResult`, `ImportResult`) and
    `hub-api.ts` (`NOT_YET`: `importSessions` and `commitImport` are still fakes on a real hub).
- **Suggested agent:** Codex, or any coding agent.

## Goal

The first-run wizard's "Import sessions" step works against a real hub. A person picks **all**,
**a filter** (started since a date, some engines, some folders), or **start fresh**. They see how
many sessions that is (a dry run), confirm, and the hub then shows exactly those sessions. Nothing
is moved or copied (read in place), and the choice can be changed later.

## What to build

1. **The contract,** in `api-v1.md`:
   - a dry run returning the count for a filter;
   - a commit storing the filter on the hub and returning how many sessions are now included;
   - a read of the current filter, so Settings can show and change it later.

   Device tokens only. Say what "included" means: which routes and recaps see an excluded
   session, and that excluding never deletes anything (the runner may still read transcripts, but
   the hub hides what's excluded).
2. **The daemon and runner:**
   - store the filter;
   - apply it consistently: the session list, activity and recaps;
   - a session that starts later matches the same rules (a new session in an included folder
     appears; "start fresh" means only sessions that start after the commit);
   - changing the filter is reversible, with no re-scan needed;
   - the dry run's count equals what the commit includes.
3. **The wizard:** `hub-api.ts` implements `importSessions` and `commitImport` and drops them from
   `NOT_YET`. The step shows the count and confirms.
4. **Mock hub and conformance** on both targets: dry run and commit agree; each mode works;
   excluded sessions are hidden and come back when the filter widens; an agent token gets 403.

## Acceptance

- fmt, clippy with `-D warnings`, the tests of the crates touched, `cargo test -p pitcrew-protocol
  --features ts`, `npm test`, the UI's checks (the first-run e2e included), both conformance
  targets, and the guards pass. Every CI job passes on the pull request.

## Out of scope

"Draft the board from history" (it needs a model and a cost prompt), the hooks step, machine
checks and sign-in (the other `NOT_YET` calls: later briefs).
