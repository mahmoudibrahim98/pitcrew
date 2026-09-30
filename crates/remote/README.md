# pitcrew-remote

Remote machines: SSH connection manager, helper deployment, launchers (direct, tmux, systemd, SLURM), tunnels, reconnect.

**Owned by stream J** — see [docs/build/streams/J.md](../../docs/build/streams/J.md).

## What is here

Everything goes through the user's own **system OpenSSH** and `~/.ssh/config`.

- `list_hosts()` lists concrete `Host` names from `~/.ssh/config` and its `Include`s (cycles
  skipped, at most 256 files). `Ssh::resolve(host)` asks `ssh -G` what a host means; the config
  is never reinterpreted here.
- `Ssh::run(host, argv)` runs `ssh [options] -- <host> <command>`.
  - **Any login shell.** The command is sent as
    `/bin/sh -c 'eval "$(printf "\ooo…")"'`, every byte of the POSIX-quoted command line an
    octal escape. What the login shell sees has no `\\`, `\'`, `!`, newline or stray `$`, so
    sh, bash, dash, zsh, ksh, fish, csh and tcsh all hand the same script to `/bin/sh`.
  - **Host names** are limited to `A-Z a-z 0-9 . _ : % [ ] @ -`, and may not start with `-`
    before or after `@`.
  - **Options:** agent and X11 forwarding, local commands, config forwardings and
    `RemoteCommand` are off; host keys are confirmed (`StrictHostKeyChecking=ask`).
    `ClearAllForwardings=yes` also clears `-L`/`-R`/`-D`, so tunnels will need their own option
    set. `-o` options do not reach `ProxyJump` hops, which read only the user's config.
  - **Unix:** connections are reused (`ControlMaster=auto`, `ControlPersist=10m`) through
    sockets in a private 0700 directory: `$XDG_RUNTIME_DIR/pitcrew-ssh`, else
    `/tmp/pitcrew-ssh-<uid>`, else `~/.pitcrew/s` if another user squatted the `/tmp` name.
  - **Windows:** its OpenSSH has no ControlMaster, so every call connects and authenticates
    anew. A persistent channel comes with the tunnel work.
  - **Errors:** ssh's own messages go to a log (`-E`) apart from the remote stderr, so exit 255
    is explained by what ssh said, never by what the command printed.
  - `Ssh::run_limited` adds an output cap and a timeout that pauses while a prompt is open.
- **Askpass bridge.** With `Ssh::with_prompts(askpass, handler)`, ssh runs `pitcrew-askpass`
  for passwords, passphrases, one-time codes and host keys; it asks the desktop's
  `PromptHandler` over a private socket (a named pipe on Windows). Both ends prove a per-call
  key first. Answers are never stored or logged. Needs OpenSSH 8.4+ on the local machine.
  Without a handler, calls run in `BatchMode` and fail instead of prompting.
  - The handler is async and gets a `PromptCancel` that fires when the prompt goes stale (ssh
    closed it, or the call ended); the dialog should close then.
  - **Cancel** stops ssh (on Unix its whole process group, askpass and jump hosts included)
    before askpass hears back, so ssh never sends the empty password it would after an
    askpass failure. Later prompts in that call are not shown.
  - `PromptKind` is a hint: server-written prompts (`(user@host) …`) are never classed as a
    passphrase or host key, Accept is refused for secrets and Text for yes/no questions. The
    UI must show the raw prompt, escaped.
- `Ssh::probe(host)` runs one POSIX-sh script and returns `MachineInfo` plus `$HOME`, the tmux
  version, `sbatch`/`squeue`, and the filesystem type of `$HOME`. The report needs this call's
  random markers and exit 0; output is capped at 1 MiB and the call at 30 s. An unknown
  filesystem counts as possibly networked.

## Tests

`cargo test -p pitcrew-remote` runs:
- unit tests, including property tests of the quoting (a POSIX lexer model, and models of
  how POSIX, fish and csh shells read the wrapper);
- `tests/fake_ssh.rs`, where the test binary acts as a scripted fake `ssh` that re-asks and
  "sends" credentials like real ssh (so a cancel test can prove no empty password goes out);
- `tests/login_shells.rs`, which runs the wrapped command through every shell it finds, or
  those listed in `PITCREW_TEST_SHELLS` (`:`-separated paths);
- `tests/real_sshd.rs`, only when `PITCREW_TEST_SSH_HOST` names a host reachable without
  prompts.
