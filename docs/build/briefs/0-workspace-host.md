# Brief 0 · Show which machine a remote workspace really is

- **Stream:** 0 · Contracts (contract, desktop and UI together). **Branch:**
  `integrator/workspace-host`.
  **Paths:**
  - `docs/build/contracts/desktop-gateway.md`;
  - `apps/desktop/src-tauri/src/{registry.rs,remote/**}` and their tests;
  - `apps/ui/src/data/**`, `apps/ui/src/shell/**`, `apps/ui/tests/**` and `apps/ui/e2e/**`.
- **First read:**
  - [README.md](README.md) and the root `AGENTS.md` (or `CLAUDE.md`);
  - `docs/build/contracts/desktop-gateway.md` ("Workspaces");
  - `apps/desktop/src-tauri/README.md` ("Remote workspaces");
  - `apps/ui/src/shell/README.md` (the switcher).
- **Suggested agent:** Codex, or any coding agent.

## Goal

The workspace switcher shows each workspace by `name`. A remote workspace's name comes from the hub
on that machine, and a hub can name itself anything: another workspace's name, or even "This
computer". The desktop knows which SSH host it connected to, because the person typed it. Show that
host next to every remote workspace's name, so a hub can't pass itself off as another machine.

## What to build

1. **The contract first:** add to `GatewayWorkspace` in `desktop-gateway.md`:
   ```ts
   host?: string;  // remote only: the SSH host the person connected, from the desktop's own
                   // records, never from the hub
   ```
   Say there that the UI must show it wherever it shows a remote workspace's name.
2. **The desktop:**
   - set `host` on every remote `GatewayWorkspace` (`registry.rs` ~171 and ~781,
     `remote/mod.rs` ~1694), from the host stored when the person added the workspace
     (`remote/mod.rs`'s config `host`);
   - leave it unset for the local workspace;
   - **never take it from anything the hub sends.** Add a test where the hub renames itself and the
     `host` stays the same.
3. **The UI:**
   - parse `host` in `apps/ui/src/data` (`GatewayWorkspace`; the parser must tolerate it missing,
     for older desktops);
   - in the switcher (`shell/sidebar.tsx` ~113), the top bar, and the remove dialog, show a remote
     workspace as its name plus its host in a quieter style. Keep the state label
     (e.g. "lab-hub · login.example.org · unreachable");
   - **tests:**
     - **render:** a remote workspace shows its host;
     - **the attack case:** a remote workspace named like the local one is still told apart by its
       host;
     - **local:** the local workspace shows no host.
   - Update the fake desktop (`data/tests/fake-desktop.ts`) and the end-to-end fixtures.
4. **Accessibility:** the host is part of the menu item's accessible name, and the existing axe
   checks still pass.

## Acceptance

- The desktop's tests pass if the environment can build it (WebKitGTK); say if it couldn't.
- `apps/ui`: `npm run typecheck`, `npm run lint`, `npm test`, `npm run build` and the end-to-end
  tests pass.
- fmt, clippy with `-D warnings`, the guards, and every CI job on the pull request pass.

## Out of scope

Renaming workspaces, and showing the kind of connection (SSH, SLURM) beyond the host.
