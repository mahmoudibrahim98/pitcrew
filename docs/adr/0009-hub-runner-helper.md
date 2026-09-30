# 0009. Hub and runner roles; the helper is uploaded over SSH

- **Status:** Accepted
- **Date:** 2026-09-30

## Context

People install PitCrew **on their laptop**, but their agents often run elsewhere: a server, a
WSL distro, an HPC cluster behind a VPN, sometimes inside a batch job on a compute node. Those
machines may have no internet, no root, no compiler and no systemd (P6). Tools that build or
download on the remote, or assume systemd, fail there.

## Decision

- `pitcrewd` has two roles, **hub** (a workspace's shared state) and **runner** (watches and runs
  sessions on one machine). In the solo case both run in one process on the workspace's primary
  machine, connected in-process; later a runner can attach to a hub over the same authenticated
  protocol (`crates/protocol/src/runner.rs`, JSON lines).
- **The desktop deploys the helper itself.** It uploads a static musl `pitcrewd` over the user's
  own SSH connection into `~/.pitcrew/bin/<version>/`, checks its sha256 against a value compiled
  into the desktop, runs `--version`, switches atomically, and removes old versions. Nothing is
  downloaded or built on the remote.
- **Launchers** (chosen at setup, default detected): `direct` (setsid), `tmux`, `systemd-user`
  (only where it works), and `slurm`, which runs the daemon as a self-renewing batch job and
  shows the exact script before submitting. Sites that forbid SSH into compute nodes use a
  **site recipe** for the last hop.
- **Connection:** system OpenSSH with the user's config, ControlMaster reuse, and a forwarded
  unix socket; where forwarding is disabled, a stdio bridge (`ssh host pitcrewd connect`).
  Passwords and one-time codes go through askpass and are never stored. No agent forwarding.
- **Loss of contact is not death** (P5): machines and sessions become `unverifiable` /
  `unreachable`, and their last known state stays visible.

## Consequences

- Stream J owns the connection manager, deployment and launchers; stream P builds the static
  binaries and their checksums.
- The runner protocol is versioned (`PROTOCOL_VERSION`); a hub accepts runners within its range.
