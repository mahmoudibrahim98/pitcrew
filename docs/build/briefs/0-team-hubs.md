# Brief 0 · Team hubs: machine ownership and per-person permissions

- **Stream:** 0 · Contracts (auth, work model, daemon routes, UI).
  **Branch:** `integrator/team-hubs`.
  **Paths:** `crates/auth/**`, `crates/hub-work/**`, `crates/daemon/**`, `crates/api/**`,
  `crates/protocol/**` (regenerate `packages/protocol-ts`), `apps/ui/src/**` (members and machine
  settings), `docs/build/contracts/api-v1.md`, `apps/mock-hub/**`, `tests/conformance/**`, the
  threat model, and the READMEs of what you touch.
- **First read:**
  - [README.md](README.md) and the root `AGENTS.md` (or `CLAUDE.md`);
  - the threat model's T79 (dispatch: a person runs only their own agents) and T80 (the files API:
    any person can use any location on the hub's machine — "a multi-person hub will need machine
    ownership");
  - `api-v1.md` (members, machines, dispatch, files, terminals, sessions).
- **Suggested agent:** an Opus-class agent. **Run it after the briefs that touch the same routes**
  (onboarding, sync, remote files) have merged, to avoid conflicts.

## Goal

Today a hub is effectively one person's: every person token can drive every machine. Make a hub safe
to share with a team.

## What to build

1. **Machine ownership:** each machine has an owner (the person who added it; the hub's own machine
   belongs to the workspace owner), and an access list the owner manages (use / admin).
2. **Permissions enforced on every route that acts on a machine:** dispatch and session commands,
   terminals, the files API, scans, hooks — a person without access gets 403. Reading the work model
   (projects, tasks, recaps) stays shared within the workspace.
3. **Roles:** workspace owner, members; who can invite, add machines, manage access. Agent tokens
   keep their current narrow rights.
4. **Migration:** existing hubs give every machine to the workspace owner, so a solo hub behaves
   exactly as before.
5. **UI:** machine access in Members/Settings; clear "not yours" states instead of failures.
6. **Tests:** a two-person hub across every machine-acting route (allowed, 403), the migration, and
   conformance on both targets; update T79 and T80.

## Acceptance

- fmt, clippy with `-D warnings`, the tests of the crates touched, `cargo test -p pitcrew-protocol
  --features ts`, `npm test`, the UI's checks, both conformance targets, and the guards pass. Every
  CI job passes on the pull request.
