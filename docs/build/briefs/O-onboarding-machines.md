# Brief O · Onboarding: machine checks, helper install and agent sign-in

- **Stream:** O · Onboarding (with the remote crate, the desktop gateway and the daemon).
  **Branch:** `integrator/onboarding-machines`.
  **Paths:** `apps/ui/src/onboarding/**`; `crates/daemon/**`, `crates/remote/**` (checks and the
  deploy progress), `apps/desktop/src-tauri/src/gateway/**` and `src/remote/**` (remote workspaces);
  `crates/protocol/**` (regenerate `packages/protocol-ts`); `docs/build/contracts/api-v1.md` and
  `docs/build/contracts/desktop-gateway.md`; `apps/mock-hub/**`, `tests/conformance/**`; the
  threat model; the READMEs of what you touch.
- **First read:**
  - [README.md](README.md) and the root `AGENTS.md` (or `CLAUDE.md`);
  - `docs/build/streams/O.md` item 1 ("machine check with a fix button per row … install the helper
    (launcher, SLURM preview, live log) → sign in to agents (runs each CLI's own login in a terminal
    on that machine)") and item 2 (reused when adding machines);
  - `crates/remote/README.md` (connection, helper deployment, launchers, site recipes);
  - `apps/ui/src/onboarding/api.ts` (`checkMachine`, `fixMachineRow`, `launcherOptions`,
    `streamInstallHelper`, `agentAccounts`, `startSignIn`) and `hub-api.ts` (`NOT_YET`).
- **Suggested agent:** an Opus-class agent (it spans the daemon, the remote crate and the desktop).

## Goal

The wizard's machine steps work for real: check a machine (this computer, a WSL distro, an SSH host,
an HPC login node), fix what's fixable, install the helper with a live log, and sign in to each
agent CLI by running **the CLI's own login** in a terminal on that machine. PitCrew never reads or
copies a login.

## What to build

1. **Check:** per machine, rows for each agent CLI and its version, tmux, git and gh, free disk,
   and SLURM where present, each with ok / warn / missing and a short reason. The hub's own machine
   from the daemon; remote machines through the existing connection (desktop gateway for
   desktop-managed workspaces).
2. **Fix:** a fix action only where it is safe and local to PitCrew (for example installing the
   helper, or opening the CLI's install page); never installing system packages silently. Say which
   rows get a fix and why.
3. **Helper install:** stream the existing deployment's progress (upload, verify, launcher, SLURM
   preview) to the wizard as lines, ending in success or a clear error.
4. **Sign-in:** list each CLI's account state as the CLI reports it (signed in or not, without
   reading token files), and start a terminal on that machine running the CLI's own login command;
   the console's terminal view shows it.
5. **The wizard:** implement those calls in `hub-api.ts` (and the gateway for remote workspaces) and
   drop them from `NOT_YET`; the machine-setup wizard reuses them when adding a machine later.
6. **Mock hub and conformance** for the hub routes; tests with fake machines and temporary homes.

## Acceptance

- fmt, clippy with `-D warnings`, the tests of the crates touched (and the desktop crate),
  `cargo test -p pitcrew-protocol --features ts`, `npm test`, the UI's checks, both conformance
  targets, and the guards pass. Every CI job passes on the pull request.
- A threat-model row for sign-in terminals (nothing reads tokens) and fix actions.

## Out of scope

Integrations (GitHub/Jira setup is brief G-sync-wiring), installing system packages.
