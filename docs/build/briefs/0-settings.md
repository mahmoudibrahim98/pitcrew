# Brief 0 · Settings

- **Stream:** 0 · Composition root (shell UI, daemon routes).
  **Branch:** `integrator/settings`.
  **Paths:** `apps/ui/src/settings/**` (new feature), `apps/ui/src/shell/**` (the route, the nav
  entry, the existing updater section), `apps/ui/src/data/**`, `crates/daemon/**`,
  `crates/hub-work/**`, `crates/protocol/**` (regenerate `packages/protocol-ts`),
  `apps/mock-hub/**`, `tests/conformance/**`, `docs/build/contracts/api-v1.md`, and the READMEs of
  what you touch. Mechanical edits elsewhere are fine; say which in the report.
- **First read:**
  - [README.md](README.md) and the root `AGENTS.md` (or `CLAUDE.md`);
  - the existing desktop-only Settings (`apps/ui/src/shell/updates.tsx`);
  - the onboarding APIs for hooks, safety, machine checks and sign-in (#55, #56);
  - the integrations UI (#52).
- **Suggested agent:** Codex.

## Goal

Today Settings exists only in the desktop app and holds only "Desktop updates". Density, hooks and
safety can be chosen only during onboarding and never changed afterwards. Make Settings the one
place to change how PitCrew works, in the browser build and the desktop.

## What to build

A Settings page with a section list on the left and deep links per section (`settings/<section>`):

1. **Profile:** name, handle, avatar initials and colour.
2. **Workspace:** name, the owner, and the data folder (read-only, with a copy button).
3. **Machines:** this computer and connected ones, their check results and fixes (from #55), and
   remove for remote ones (the desktop's existing call).
4. **Agents:**
   - detected CLIs per machine, with version and sign-in state, and sign in again (#55);
   - default agents (from #60), each editable.
5. **Hooks:** status per engine (installed, missing, conflicting), with "Review and install" (the
   onboarding diff flow) and "Remove", the latter showing its diff first.
6. **Safety:** the default permission mode and the automatic-acceptance budget, in plain words
   ("Let PitCrew accept low-risk changes automatically, up to N an hour").
7. **Integrations:** links to the GitHub and Jira settings from #52.
8. **Appearance:** theme and density.
9. **Notifications:** which events notify, and desktop notifications on or off.
10. **Keyboard shortcuts:** the list, from the shell's `SHORTCUTS`.
11. **Updates** (desktop only; the existing section, moved here) and **About**: versions of the app,
    the daemon and the protocol, and where the logs are.

Settings sits in the sidebar footer for everyone (a gear icon), and the palette has
"Open settings" commands per section. Add routes only where an API is missing, with mock-hub
parity and conformance.

## Acceptance

- Everything chosen in onboarding can be changed in Settings, and the change takes effect without
  a restart.
- fmt, clippy with `-D warnings`, the tests of the crates touched, `cargo test -p pitcrew-protocol
  --features ts`, `npm test`, the UI's checks, both conformance targets, and the guards pass. Every
  CI job passes on the pull request.
