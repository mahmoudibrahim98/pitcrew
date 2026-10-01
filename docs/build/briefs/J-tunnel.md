# Brief J · The tunnel: reaching a remote daemon, and staying connected

- **Stream:** J · Remote and HPC. **Branch:** `s/J/tunnel`. **Paths:** `crates/remote/**`
  (+ `Cargo.lock`).
- **First read:** [README.md](README.md), [J-slurm.md](J-slurm.md) and
  [J-deploy.md](J-deploy.md) (merged, with their reviews in mind), `docs/build/streams/J.md`
  (work packages 5 and 6), ADR-0009, ADR-0006, the `crates/remote` README, and
  [the desktop gateway contract](../contracts/desktop-gateway.md) (what the desktop will need from
  you).

## Goal

The laptop gets a byte stream to the person's `pitcrewd` on a remote machine (a login node, or a
compute node inside a SLURM job) over system OpenSSH. It notices quickly when the stream is lost,
and recovers by itself. The desktop's gateway (stream K) will use this for remote workspaces in its
next brief.

## What to build

1. **A `Connector`** in `pitcrew-remote`, the public interface stream K builds on:
   - `connect() → an async read/write stream` to the remote daemon's socket;
   - `state()` and a watch of `connected` / `unverifiable` / `unreachable`, with the reason;
   - `close()`.
   - Each `connect()` is one HTTP or WebSocket connection's worth of bytes. Many may be open at
     once, so make them cheap.
2. **Two transports**, chosen per machine and remembered:
   - **Forwarded socket** (`ssh -N -L <local socket>:<remote socket>`), on a laptop that has unix
     sockets:
     - the local socket lives in a private 0700 directory, removed on close;
     - many connections share it;
     - detect when the site forbids it (`AllowStreamLocalForwarding no`; the error from ssh) and
       fall back.
   - **Stdio bridge:** each `connect()` runs `ssh … pitcrewd connect [--socket <path>]` over the
     machine's ControlMaster, so each one costs a channel, not a login.
     - On Windows, use this always, unless you find unix-socket forwarding there sound; say which.
     - `pitcrewd connect` copies stdin and stdout to the daemon's socket after the same checks a
       client makes (the socket and its directory are ours; peer uid on Unix).
     - Write that logic as a library function in `crates/remote`, e.g.
       `bridge::connect_stdio(socket_path)`. The daemon's CLI (stream 0) will call it; describe
       the one-line change in your report.
3. **Compute nodes:** the endpoint's host is a node inside a job.
   - **Re-check before connecting:** the node name has a valid shape, is the one recorded for
     this job, and the job is still ours and running.
   - **Then follow the site recipe's last hop:**
     - `ssh -J <login> <node>` where nodes take SSH;
     - otherwise run the bridge through the job:
       `ssh <login> srun --jobid <id> --overlap pitcrewd connect --socket <path>`, which also
       reaches a node-local `$TMPDIR` socket.
   - Every argument is validated and quoted, as in the deploy.
4. **The reconnect ladder:**
   - **Losing the stream:** the state becomes `unverifiable` within 10 seconds. Use SSH
     keepalives (`ServerAliveInterval`/`CountMax`), a cheap probe through the transport, and the
     ControlMaster's own check.
   - **Recovering:** back off with jitter; recover without the person doing anything, after a
     VPN drop or laptop sleep (detect a wall-clock jump); and become `unreachable` with a reason
     after a bounded time.
   - **A job that ended or moved** (a new endpoint) is picked up again from the endpoint record.
   - Resuming the API stream (`since=`) is the caller's job; don't duplicate it.
   - **Askpass:** if ssh needs a password or one-time code while reconnecting, it goes through
     the existing askpass bridge. Never store it.
5. **Security, the same bar as the deploy:**
   - no agent forwarding;
   - `-o` options fixed by us; nothing from remote output reaches an ssh command line unchecked;
   - local sockets private;
   - the `ssh` child gets only the environment it needs;
   - logs never contain socket paths with user names beyond what is needed, and never secrets.

## Acceptance

- **Fake-ssh tests** (the existing harness):
  - a forwarded socket with many connections;
  - forwarding refused, then the stdio bridge;
  - the bridge through `srun --overlap` to a node-local socket;
  - ProxyJump to a node;
  - a node name that fails the re-check, which is refused before any ssh call;
  - a dropped stream, becoming `unverifiable` in under 10 seconds, then recovering;
  - a wall-clock jump;
  - a job that ended, making the machine `unreachable` with a reason;
  - askpass during a reconnect.
- **`bridge::connect_stdio`:** byte-exact both ways, half-close, a large transfer, refusal of a
  socket that isn't ours, and no leftover processes (keep the leak guard).
- **The real-sshd tests** (`PITCREW_TEST_SSH_HOST`) get a tunnel case. They are skipped here, as
  before.
- fmt, clippy (Linux and Windows target), rustdoc with `-Dwarnings`, the tests under the 11 shells,
  `cargo test --workspace`, and the guards all pass.

## Out of scope

The desktop using the connector (stream K's next brief), pairing, `systemd-user`, WSL machines,
renewing or handing over SLURM jobs, and the daemon CLI change itself.
