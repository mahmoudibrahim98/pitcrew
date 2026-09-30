# Brief O · Onboarding wizard components

- **Stream:** O · Onboarding and import. **Branch:** `s/O/onboarding-components` (based on
  `s/L/shell`, the shell with feature registration, until it merges). **Paths:**
  `apps/ui/src/onboarding/**` only.
- **First read:** [README.md](README.md), then `docs/build/streams/O.md` (work packages 1 and 2),
  ADR-0009 and ADR-0010, and in your worktree: `apps/ui/src/shell/README.md` (**feature
  registration**), `apps/ui/src/data/README.md` (**use `useLiveQuery`**), `apps/ui/src/design`,
  and `docs/build/contracts/api-v1.md`.

## Goal

The first-run wizard and the add-a-machine wizard, as a registered feature with routes and tested
components. Several backend APIs (the machine check, helper install, scan, CLI sign-in) don't
exist yet, so **define a typed client interface for them inside `src/onboarding`**, with an
in-memory fake that behaves plausibly, and build the UI against it. The integrator turns those
types into the real contract later.

## What to build

1. **An `OnboardingApi` interface** (TypeScript) with a fake implementation:
   - `checkMachine`: CLI versions, tmux, git and gh, disk, SLURM;
   - `installHelper`: streamed progress lines, the launcher choice, and a SLURM script preview;
   - `agentAccounts`, and `startSignIn`, which returns a terminal session id;
   - `scan`: streamed progress, then counts per engine, folder and month, plus suggested projects
     and workstreams;
   - `importSessions`: a filter and a dry-run count;
   - `hooksDiff` and `installHooks`;
   - `saveSafety`.

   Document each call's shape in `src/onboarding/README.md` as the proposed contract.
2. **Wizard steps**, each its own component with keyboard navigation and a skip where allowed:
   1. Welcome (theme and density).
   2. First workspace (name; primary machine: this computer, WSL distro, or SSH host picked from
      a list).
   3. Machine check, with a fix button per row.
   4. Install helper, with the launcher and, for SLURM, the **exact script preview**, then a
      live log.
   5. Sign in to agents: one row per engine and account; "Sign in" opens the console terminal,
      a placeholder link for now.
   6. Integrations (skippable).
   7. Scan, with streamed progress.
   8. Create projects and workstreams from the suggestions: tick, rename, move a workstream
      between projects; template Research, Software or Blank.
   9. Import sessions (all, filtered, or start fresh; read in place, reversible).
   10. Hooks, showing the exact diff before installing.
   11. Safety: permission mode, and back-office on/off with caps.
   12. Done, then Home.
3. **Register the feature:** routes `/w/$ws/onboarding/...` and `/onboarding` (first run, before
   a workspace exists), plus an "Add a machine" palette command.
4. The add-a-machine wizard reuses steps 2–5 and 7–9.

## Acceptance

- Vitest and Testing Library over the fake: each step renders, validates and advances; skips
  work.
- Playwright on port **47431** (hub) and **5431** (UI): the whole wizard runs end to end, and
  the Scan → Create step produces the ticked projects.
- axe reports no violations on every step, light and dark.
- `typecheck`, `lint` and `build` pass; onboarding code is lazy-loaded.

## Out of scope

The real backend calls and the legacy importer (a later brief).
