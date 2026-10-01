# pitcrew-remote

Remote machines: SSH connection manager, helper deployment, launchers (direct, tmux, systemd, SLURM), tunnels, reconnect.

**Owned by stream J** — see [docs/build/streams/J.md](../../docs/build/streams/J.md).

## What is here

Everything goes through the user's own **system OpenSSH** and `~/.ssh/config`.

- `list_hosts()` lists concrete `Host` names from `~/.ssh/config` and its `Include`s (cycles
  skipped, at most 256 files). `Ssh::resolve(host)` asks `ssh -G` what a host means; the config
  is never reinterpreted here. `ssh -G` runs the config's `Match exec` commands, so it is
  bounded (`RESOLVE_LIMITS`: 10 s, 1 MiB).
- `Ssh::run(host, argv)` runs `ssh [options] -- <host> <command>`.
  - **Any login shell.** The command is sent as
    `/bin/sh -c 'eval "$(printf "\ooo…")"'`, every byte of the POSIX-quoted command line an
    octal escape. What the login shell sees has no `\\`, `\'`, `!`, newline or stray `$`, so
    sh, bash, dash, zsh, ksh, fish, csh and tcsh all hand the same script to `/bin/sh`.
  - **xonsh is unsupported and unsafe** as a login shell: it may decode `\ooo` itself, and the
    `printf` layer would then read a `\047` from the command as a real quote, so argv could
    break out. `Ssh::probe` reads `$SHELL` and refuses such hosts (`SshError::UnsupportedShell`).
  - **Length:** a wrapped command may be 128 KiB on Unix (Linux's limit for one argument) and
    30,000 characters on Windows (`CreateProcess` takes 32,767 for the whole command line);
    longer ones are refused before ssh starts.
  - **Host names** are limited to `A-Z a-z 0-9 . _ : % [ ] @ -`, and may not start with `-`
    before or after `@`.
  - **Options:** agent and X11 forwarding, local commands, config forwardings and
    `RemoteCommand` are off; host keys are confirmed (`StrictHostKeyChecking=ask`).
    `ClearAllForwardings=yes` also clears `-L`/`-R`/`-D`, so tunnels will need their own option
    set. `-o` options do not reach `ProxyJump` hops, which read only the user's config.
  - **Unix:** connections are reused (`ControlMaster=auto`, `ControlPersist=10m`) through
    sockets in a private 0700 directory: `$XDG_RUNTIME_DIR/pitcrew-ssh`, else
    `/tmp/pitcrew-ssh-<uid>`, else `~/.pitcrew/s`. A candidate that is squatted, too long for a
    socket path, or has characters `ControlPath` would expand is skipped.
  - **Windows:** its OpenSSH has no ControlMaster, so every call connects and authenticates
    anew. A persistent channel comes with the tunnel work.
  - **Stopping:** a cancel, a timeout or a dropped call stops ssh and everything it started
    (askpass, `ProxyJump` hops, `Match exec`): its process group on Unix, its **Job Object** on
    Windows (`JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE`, so the OS also ends it if PitCrew dies). The
    Job Object needs four Win32 calls; `src/job.rs` is the crate's only unsafe code.
  - **Errors:** ssh's own messages go to a log (`-E`, at `LogLevel=ERROR`) apart from the remote
    stderr. Exit 255 is an error only when that log shows ssh failing, and its kind comes only
    from ssh's own message formats, matched as whole lines. ssh logs server text without
    escaping newlines, so reading stops at ssh's terminal message (e.g. "Permission denied
    (…).", whose method list is the server's) and at lines carrying server text (a disconnect
    reason, an algorithm offer, a refused channel). Informational lines never turn a remote
    command's own 255 into an error.
  - `Ssh::run_limited` adds an output cap and a timeout that pauses while a prompt is open.
- **Askpass bridge.** With `Ssh::with_prompts(askpass, handler)`, ssh runs `pitcrew-askpass`
  for passwords, passphrases, one-time codes and host keys; it asks the desktop's
  `PromptHandler` over a private socket (a named pipe on Windows, which the client opens at
  identification level only). Both ends prove a per-call key first. Answers are never stored
  or logged. Needs OpenSSH 8.4+ on the local machine. The askpass path must be absolute and
  exist (a missing one would fail every prompt). Without a handler, calls run in `BatchMode`
  and fail instead of prompting.
  - The handler is async and gets a `PromptCancel` that fires when the prompt goes stale (ssh
    closed it, or the call ended); the dialog should close then.
  - **ssh never sees askpass fail**, since it would then send an empty password and ask again:
    - **Cancel** stops ssh and everything it started before askpass hears back. Later prompts
      in that call are not shown. (For a yes/no question, Cancel simply answers "no".)
    - If the bridge closes without an answer (PitCrew quit or crashed, or the handshake
      failed), `pitcrew-askpass` kills the ssh that asked, but only while it is still its
      parent and shares its process group, so never a reaper that adopted it (pinned with a
      pidfd on Linux). On Windows it waits instead, and the Job Object ends both.
    - While PitCrew runs, a client that says hello and then fails the handshake stops the
      call with `SshError::Bridge` (on Windows by ending the job); so does ssh being killed
      by its askpass.
  - `PromptKind` is a hint: server-written prompts (`(user@host) …`) are never classed as a
    passphrase or host key, Accept is refused for secrets and Text for yes/no questions. The
    UI must show the raw prompt, escaped. `UpdateHostKeys=ask`'s "Accept updated hostkeys?" is
    a yes/no question (Confirm), so the user's choice of key rotation is kept.
- `Ssh::probe(host)` runs one POSIX-sh script and returns `MachineInfo` plus `$HOME`, the tmux
  version, `sbatch`/`squeue`, the login shell, and the filesystem type of `$HOME`. The report
  needs this call's random markers and exit 0; output is capped at 1 MiB and the call at 30 s.
  Only filesystems on an allowlist of local types (ext2/3/4, xfs, btrfs, zfs, tmpfs, f2fs,
  bcachefs, overlayfs, apfs, hfs and similar) count as local; anything else, including
  `UNKNOWN (0x…)`, every FUSE filesystem (`fuseblk`), 9p, virtiofs and vboxsf, counts as
  possibly networked.
- `Ssh::run_with_input` streams bytes to the remote command's stdin while reading its output,
  with progress, under the same limits.

## The helper on a machine (`helper`)

Deploys the static `pitcrewd` over the user's ssh and starts it. Nothing on the machine needs
internet, root, a compiler or a package manager: only a POSIX `sh`, common tools (`dd`, `ls`,
`awk`, `sed`, `find -mmin`, `readlink`, `date +%s`, …) and one of `sha256sum`, `shasum` or
`openssl`.

```rust
let probe = ssh.probe(host).await?;
let target = Target::new(ssh, host, &probe)?;           // refuses unknown platforms
// The desktop picks `target.platform().artefact()` (e.g. pitcrewd-x86_64-unknown-linux-musl)
// and the sha256 compiled in for it.
let helper = Helper::new(target.platform(), VERSION, SHA256, bytes)?;
deploy(&target, &helper, &DeployOptions::default()).await?;
let started = DirectLauncher::default().start(&target).await?;   // or TmuxLauncher
```

- **Platforms:** Linux x86_64 and aarch64 (static musl) and macOS (universal), from the probe's
  `uname -s`/`uname -m`. Anything else (FreeBSD, 32-bit ARM, POWER, …) is refused by name, and a
  helper built for another platform is refused before any call. `Helper::new` checks the
  version, the size (at most 256 MiB) and that the bytes hash to the expected sha256.
- **Layout:** `~/.pitcrew/bin/<version>/pitcrewd`, `bin/current -> <version>` (relative),
  `bin/previous`, `run/endpoint.json`, `run/pitcrewd.sock`, `run/pitcrewd.log`. Every directory
  must be a real directory owned by the user with mode 0700 and no ACL; one that is not is
  refused (`UnsafeDirectory`), never repaired.
- **The remote side** is one script, `src/helper/helper.sh`, sent on **stdin** (the Windows
  command-line limit leaves the shell-neutral wrapper about 7,500 bytes). The command line is a
  fixed bootstrap that reads exactly the script's length with `dd`, so the helper bytes behind it
  stay on stdin, and runs it only if it arrived whole. It reports between random markers, like
  the probe.
- **Deploy** is at most two calls, each under the `bin/.lock` lock:
  1. `check` verifies a copy already installed under the version (sha256 computed on the
     machine, then `--version`) and switches to it. The same deploy again stops here: it only
     verifies. A damaged copy is removed. A missing hash tool, an unsafe directory or a busy
     lock is found here, before anything is uploaded.
  2. `install` streams the helper into `bin/<version>/pitcrewd.tmp.<random>` under `umask 077`
     (so it is 0600 from the first byte, in 0700 directories), checks the byte count, the
     sha256 (`sha256sum`, else `shasum -a 256`, else `openssl dgst -sha256`; a tool that fails
     or prints no hash falls through) and `--version` (whose first line must have the version as
     a word), deleting the file if any fails; then `chmod 700`, a rename into place, the switch,
     and GC.
  - **Switch:** a new link made with `ln -sfn` on a temporary name, renamed over `current` with
    `mv -T` (GNU), else `mv -h` (BSD, macOS). Where `mv` has neither (busybox), `ln -sfn`
    replaces it in place and `Deployed::atomic` is false. `previous` names the version before.
  - **GC** removes every version directory but `current`'s and `previous`'s.
  - **Interrupted uploads** never land in place: the file is a temporary one until verified. The
    script removes it on any exit (and on SIGHUP, SIGPIPE, SIGTERM); if the script itself is
    killed, the next deploy sweeps it.
  - **Bounds:** `DeployOptions::timeout` per call (prompts excluded), `lock_wait`, and
    `stale_lock`, which must exceed both; `progress` reports bytes handed to ssh.
- **Locks** are `mkdir` directories with an `owner` file (host, pid, call tag). A lock is stale
  when older than the limit, or taken on this host by a process that is gone (so a killed deploy
  does not block the next one for long). A stale lock is moved aside atomically and removed; a
  holder checks it still owns its lock before every change, so a lock broken by mistake stops
  its old holder (`LockLost`) instead of letting two runs write.
- **Launchers** implement `Launcher` (object-safe; the SLURM launcher will be another):
  - `DirectLauncher`: `setsid nohup` (`nohup` alone where there is no `setsid`, as on macOS),
    double-forked so the helper is nobody's child;
  - `TmuxLauncher`: the session `pitcrew-helper` on its own tmux server (`tmux -L
    pitcrew-helper -f /dev/null`), only with tmux 3.2 or newer (`TmuxLauncher::new` refuses
    older, missing or unreadable versions).

  Both run `bin/current/pitcrewd serve --listen unix:<root>/run/pitcrewd.sock` (or
  `LaunchOptions::args`), append its output to `run/pitcrewd.log`, wait for the socket
  (`ready_timeout`; a helper that exits or never binds is reported with the log's last lines,
  and stopped), and write `run/endpoint.json` atomically:
  `{"pid":…,"host":…,"version":…,"started":…,"launcher":…,"socket":…}`. `status` reports
  whether it runs and which version is installed; `stop` sends SIGTERM, then SIGKILL after
  `stop_timeout`. All are idempotent. A pid counts only while alive, not a zombie, and named
  `pitcrewd`, so a recycled pid is never signalled. On clusters whose login nodes share `$HOME`,
  a record from another host is reported (`OtherHost`) and never acted on, unless
  `LaunchOptions::take_over` says so.
- **Secrets:** none are involved; nothing here logs. Reports and errors carry paths and the
  first line of `--version`, with control characters replaced.

## Tests

`cargo test -p pitcrew-remote` runs:
- unit tests, including property tests of the quoting (a POSIX lexer model, and models of
  how POSIX, fish and csh shells read the wrapper);
- `tests/fake_ssh.rs`, where the test binary acts as a scripted fake `ssh` that re-asks and
  "sends" credentials like real ssh (so cancel and app-quit tests can prove no empty password
  goes out), and as a fake app that quits mid-prompt;
- `tests/login_shells.rs`, which runs the wrapped command through every shell it finds, or
  those listed in `PITCREW_TEST_SHELLS` (`:`-separated paths);
- `tests/deploy.rs` (Unix), where the test binary is a fake `ssh` that runs the real remote
  script with the local `/bin/sh` in a temporary `HOME`, with a `PATH` holding only the tools
  the script may use. It can cut, pause or corrupt the upload, or never read it. The binary also
  plays `pitcrewd` and the hash tools (in the formats of `sha256sum`, `shasum`, OpenSSL 1.1 and
  3). It covers deploy and the idempotent re-run, hash mismatch, interrupted and killed uploads,
  concurrent and stale locks, GC, every hash tool, BSD and busybox `mv`, unknown platforms,
  unsafe directories, the file modes during the upload, a stalled upload, and the direct and
  tmux launchers (start, status, stop, `endpoint.json`, failures, other hosts). It runs the
  whole flow again with each POSIX shell of `PITCREW_TEST_SHELLS` as the machine's `sh`;
- `tests/real_sshd.rs`, only when `PITCREW_TEST_SSH_HOST` names a host reachable without
  prompts; it also deploys a stand-in helper into a throwaway directory there and removes it.

The Windows code (Job Object, named pipes) is checked with clippy for
`x86_64-pc-windows-gnu`; the test suites have not run on Windows yet (the deploy tests need a
Unix `sh` and skip there).
