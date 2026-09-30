# Stream J · Remote and HPC

**Goal:** from a laptop, set up and keep a connection to any machine the user can SSH to,
including HPC clusters behind a VPN, **without anything needing internet, root, a compiler or
systemd on the remote**.

**Owns:** `crates/remote/**`.  **Depends on:** stream 0.  **Model:** Opus-class.
**Read first:** ADR-0009, ADR-0006; principle P6 (nothing on a remote needs internet, a
compiler, root, a package manager or systemd).

## Work packages

1. **Connection manager** over **system OpenSSH** with the user's config: host list from
   `~/.ssh/config`, ControlMaster reuse, an **askpass bridge** so passwords and one-time codes
   are asked in the desktop and never stored, host-key confirmation as a trust dialog,
   **no agent forwarding**.
2. **Helper deployment:** detect OS and architecture; upload the static `pitcrewd` into
   `~/.pitcrew/bin/<version>/`; verify its sha256 against the value compiled in; run
   `--version`; switch atomically; garbage-collect old versions.
3. **Launchers:** `direct` (setsid), `tmux`, `systemd-user` (only when it works and lingering is
   allowed), and `slurm`: a self-renewing batch job whose script the user sees before submit
   (account, partition, time), writing its endpoint to `~/.pitcrew/run/endpoint.json`.
4. **Site recipes:** a small trait for the last hop to a compute node on sites that forbid
   direct SSH there (for example a site tool that starts a per-user SSH server inside the job).
   Ship one generic recipe and document how to add one.
5. **Tunnel:** `ssh -L` to the daemon's unix socket; where forwarding is disabled, a **stdio
   bridge** (`ssh host pitcrewd connect`). API traffic from compute nodes without internet goes
   through the same path.
6. **Reconnect ladder** and the `unverifiable` / `unreachable` states: back-off, resume by stream
   revision, survive laptop sleep, VPN drops and job hand-over.
7. **WSL** machines (`wsl.exe -d <distro>`) through the same launcher model.

## Acceptance

- Tests against a fake `ssh` on `PATH` (scripted responses) cover deploy, verify, switch and GC.
- CI Linux runs an end-to-end test against a local `sshd` in a container (optional job).
- SLURM script generation is snapshot-tested; nothing is submitted without the preview step.
- Connection loss marks machines `unverifiable` within 10 s and recovers without user action.

## Do not

Store passwords or keys; enable agent forwarding; download anything on the remote; assume root.
