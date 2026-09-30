# pitcrew-remote

Remote machines: SSH connection manager, helper deployment, launchers (direct, tmux, systemd, SLURM), tunnels, reconnect.

**Owned by stream J** — see [docs/build/streams/J.md](../../docs/build/streams/J.md).

## What is here

Everything goes through the user's own **system OpenSSH** and `~/.ssh/config`.

- `list_hosts()` lists concrete `Host` names from `~/.ssh/config` and its `Include`s.
  `Ssh::resolve(host)` asks `ssh -G` what a host means; the config is never reinterpreted here.
- `Ssh::run(host, argv)` runs `ssh [options] -- <host> <command>`. Every argument is quoted
  with POSIX single quotes; host names that look like options are refused. Agent and X11
  forwarding, local commands and config forwardings are off; host keys are confirmed
  (`StrictHostKeyChecking=ask`).
  - **Unix:** connections are reused (`ControlMaster=auto`, `ControlPersist=10m`) through
    sockets in a private 0700 directory.
  - **Windows:** its OpenSSH has no ControlMaster, so every call connects and authenticates
    anew. A persistent channel comes with the tunnel work.
- **Askpass bridge.** With `Ssh::with_prompts(askpass, handler)`, ssh runs `pitcrew-askpass`
  for passwords, passphrases, one-time codes and host keys; it asks the desktop's
  `PromptHandler` over a private socket (a named pipe on Windows). Both ends prove a per-call
  key first. Answers are never stored or logged. Needs OpenSSH 8.4+ on the local machine.
  Without a handler, calls run in `BatchMode` and fail instead of prompting.
- `Ssh::probe(host)` runs one POSIX-sh line and returns `MachineInfo` plus `$HOME`, the tmux
  version, `sbatch`/`squeue`, and the filesystem type of `$HOME`.

## Tests

`cargo test -p pitcrew-remote` runs the unit tests (including a property test of the quoting
against a POSIX lexer model) and `tests/fake_ssh.rs`, where the test binary acts as a scripted
fake `ssh`. `tests/real_sshd.rs` runs against a real server only when
`PITCREW_TEST_SSH_HOST` names a host reachable without prompts.
