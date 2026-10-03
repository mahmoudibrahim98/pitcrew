# Brief 0 · WSL distros as machines

- **Stream:** 0 · Contracts (stream J's remote crate, stream K's desktop, and the connect wizard
  together). **Branch:** `integrator/wsl-machines`.
  **Paths:**
  - `docs/build/contracts/desktop-gateway.md`;
  - `crates/remote/**`;
  - `apps/desktop/src-tauri/src/**` and its tests;
  - `apps/ui/src/onboarding/connect/**` and `apps/ui/src/data/**`, with their tests;
  - `docs/security/threat-model.md`;
  - the lockfiles.
- **First read:**
  - [README.md](README.md) and the root `AGENTS.md` (or `CLAUDE.md`);
  - `docs/build/streams/J.md` (item 7);
  - `crates/remote`'s README (connection, deployment, launchers, the stdio bridge
    `pitcrewd connect`);
  - `apps/desktop/src-tauri`'s README ("Remote workspaces");
  - `desktop-gateway.md` (`gateway_remote_probe`, `gateway_remote_plan`, `gateway_remote_add`);
  - the threat model's remote rows.
- **Suggested agent:** Codex on a Windows machine with WSL. The real-WSL test needs one; everything
  else also runs on Linux and in CI.
- **Overlaps:** PR #17 adds `host` to `GatewayWorkspace`. Rebase onto it if it has merged; show a
  WSL workspace's host as `wsl:<distro>`.

## Goal

A person on Windows can add a WSL distro on their own computer as a machine, the way they add an SSH
host today. PitCrew then deploys its helper into the distro and tracks the agents running there.

## What to build

1. **The contract first:**
   - `gateway_wsl_distros() → { available: boolean, distros: { name, default, running, version }[] }`;
   - a WSL target in `gateway_remote_probe`, `gateway_remote_plan` and `gateway_remote_add` (for
     example `target: { kind: 'wsl', distro }` next to the SSH host form). Keep the SSH form
     unchanged.
   - Say in the contract what differs from SSH:
     - no askpass and no host keys;
     - the transport is `wsl.exe -d <distro> --`;
     - the API goes over the stdio bridge.
2. **The remote crate:** a WSL transport next to SSH, behind the same seam the deploy, launch and
   tunnel code already uses. If there isn't one, introduce the smallest one that fits.
   - **Commands:** run through `wsl.exe -d <distro> --exec sh -c …`, or `--` with careful quoting.
     Test quoting with names containing spaces and quotes.
   - **Platform detection and helper deployment** work as on a Linux SSH host: the musl helper, its
     sha256 against the compiled manifest, and an atomic switch.
   - **Launchers:** `direct` and `tmux`. SLURM and systemd-user don't apply.
   - **The API:** goes over the existing stdio bridge (`pitcrewd connect`) through `wsl.exe`, not an
     `ssh -L` forward.
   - **Listing distros:** `wsl.exe -l -v` prints **UTF-16LE**. Parse it robustly (BOM, `*` for the
     default, spaces in names). "WSL not installed" is a normal answer, not an error.
3. **The desktop:**
   - the gateway commands above;
   - a WSL workspace in the registry;
   - reconnect after `wsl --shutdown` or a reboot, with the same ladder and states as SSH.
4. **The UI:** the connect wizard offers "A WSL distro on this computer" when `available` is true. It
   lists the distros and walks through probe, plan and add, as the SSH path does.
5. **The threat model:** a row for WSL.
   - The distro runs as the person, but other Windows processes of the same person can reach it.
   - Say what the helper's private-directory checks mean inside it.

## Tests

- **Everywhere, including CI on Linux and macOS:**
  - a fake `wsl.exe` (a configurable program path) that records its arguments and plays recorded
    output: UTF-16 listings, missing WSL, a stopped distro;
  - the plan, add and reconnect flows against it.
- **A real-WSL test, opt-in:** it runs only when `PITCREW_TEST_WSL_DISTRO=<name>` is set.
  - It runs against that distro with `HOME` pointed at a fresh folder under `/tmp` inside it, and
    removes the folder afterwards.
  - Report the result in the pull request.

## Hard rules on a real machine

- **Never** run any of these:
  - `wsl --shutdown`, `--terminate`, `--unregister`, `--install`, `--set-default`, `--update`,
    `--import` or `--export`;
  - `sudo` or a package manager inside a distro.
- Never edit `.wslconfig` or `/etc/wsl.conf`.
- Never touch the distro's real `~/.pitcrew`, `~/.claude`, `~/.codex` or OpenCode folders. Tests use
  the temporary `HOME` only.

## Acceptance

- The fake-`wsl.exe` tests pass on Linux and Windows.
- The real-WSL test passes on the Windows machine. Report its output.
- fmt, clippy with `-D warnings` (the workspace and the desktop crate), `npm test`, the UI's checks,
  the guards, and every CI job pass.

## Out of scope

Running Windows-side agents inside WSL, WSL1 (say what happens if a distro is version 1), and
installing WSL.
