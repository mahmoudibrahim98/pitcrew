# Brief O · Onboarding: the hooks and safety steps on a real hub

- **Stream:** O · Onboarding (with the daemon routes it needs).
  **Branch:** `integrator/onboarding-hooks-safety`.
  **Paths:** `apps/ui/src/onboarding/**`; `crates/daemon/**`, `crates/cli/**` (only to reuse the
  hook installer as a library), `crates/hub-work/**` (only the safety settings), `crates/protocol/**`
  (regenerate `packages/protocol-ts`); `docs/build/contracts/api-v1.md`; `apps/mock-hub/**`,
  `tests/conformance/**`; the READMEs of what you touch.
- **First read:**
  - [README.md](README.md) and the root `AGENTS.md` (or `CLAUDE.md`);
  - `docs/build/streams/O.md` item 1 ("hooks (diff first) → safety (permission mode, back-office
    caps)") and `docs/adr/0010-real-cli-sessions.md` (hooks installed only after the user sees the diff);
  - [I-hook-install.md](I-hook-install.md), [I-hook-exec-form.md](I-hook-exec-form.md) and
    `crates/cli/src/install/**` (the installer, its diffs, CRLF handling);
  - `apps/ui/src/onboarding/api.ts` (`HooksDiffFile`, `hooksDiff`, `installHooks`, `saveSafety`)
    and `hub-api.ts` (`NOT_YET`).
- **Suggested agent:** Codex, or any coding agent.

## Goal

The first-run wizard's **Hooks** and **Safety** steps work against a real hub. Hooks are what make
sessions show live states. Nothing changes in an agent CLI's configuration until the person has
seen the exact diff and confirmed.

## What to build

1. **Hooks, through the hub that runs on the machine** (the hub's own machine; other machines answer
   501 for now):
   - a route returning, for each agent CLI found (Claude Code, Codex, OpenCode), the file, its text
     before and after (`null` before for a new file) — computed by the same code as
     `pitcrew hooks install --dry-run`;
   - a route applying exactly that change, refusing if the file changed since the diff was made
     (a revision, as the files API does), idempotent, with the CLI installer's backup behaviour;
   - device tokens only (agents get 403); never log file contents; never touch CLI logins.
2. **Safety:** a route to read and save the workspace's default permission mode (the CLI's own by
   default; skipping permissions is an explicit opt-in with a warning) and the back office's caps,
   stored in the work model with an event.
3. **The wizard:** `hub-api.ts` implements `hooksDiff`, `installHooks` and `saveSafety` and drops
   them from `NOT_YET`; the Hooks step shows the diff file by file and installs only on confirm.
4. **Mock hub and conformance** on both targets; tests with temporary agent homes only.

## Acceptance

- fmt, clippy with `-D warnings`, the tests of the crates touched, `cargo test -p pitcrew-protocol
  --features ts`, `npm test`, the UI's checks (the first-run e2e included), both conformance
  targets, and the guards pass. Every CI job passes on the pull request.

## Out of scope

Machine checks, helper install and sign-in (brief O-onboarding-machines), hooks on remote machines.
