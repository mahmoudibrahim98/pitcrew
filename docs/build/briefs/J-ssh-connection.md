# Brief J · SSH connection and machine probe

- **Stream:** J · Remote and HPC. **Branch:** `s/J/ssh-connection`. **Paths:**
  `crates/remote/**` only.
- **First read:** [README.md](README.md), then `docs/build/streams/J.md`, ADR-0009, ADR-0006,
  and `MachineInfo` in `crates/protocol/src/model.rs`.

## Goal

The first layer of remote machines: talk to any host the user can already reach with their own
**system OpenSSH**, safely:
- list their hosts;
- run commands with correct quoting;
- ask for passwords and one-time codes through the desktop, never storing them;
- confirm host keys;
- probe what the machine is.

Deploying the helper and the launchers come in later briefs.

## What to build

1. **Host list.** Read `~/.ssh/config`, including `Include` globs, and list concrete `Host`
   names; skip wildcard patterns and ignore `Match` blocks with a note. Resolve a host's
   effective settings with `ssh -G <host>` (hostname, user, port, proxy jump). Never parse your
   way around ssh's own semantics.
2. **Running commands.** A `Ssh` value holds the ssh program path, so tests can inject a fake.
   `run(host, argv) -> Output` runs `ssh [options] -- <host> <quoted command>`:
   - **Quoting is security-critical:** build the remote command from `argv` with POSIX
     single-quote quoting. Reject host names starting with `-` or containing whitespace or
     control characters.
   - **Always pass:** `-o ForwardAgent=no`, `-o ForwardX11=no`, `-o PermitLocalCommand=no`,
     `-o ServerAliveInterval=15 -o ServerAliveCountMax=3`, `-o ConnectTimeout=…`, and
     `-o StrictHostKeyChecking=ask`, so the host key prompt reaches the askpass bridge.
   - **Unix:** reuse connections with `ControlMaster=auto`, `ControlPath` in a private 0700
     directory, and `ControlPersist=10m`.
   - **Windows:** its OpenSSH has no ControlMaster, so each call connects anew. Document this;
     a persistent channel comes later.
3. **Askpass bridge.** A small binary target in this crate (e.g. `pitcrew-askpass`) that ssh
   runs via `SSH_ASKPASS` + `SSH_ASKPASS_REQUIRE=force`:
   - it connects to a private local socket or pipe named in an environment variable;
   - it sends `{kind: password|passphrase|otp|host_key|confirm, prompt}`;
   - it prints the answer, or exits non-zero when cancelled.

   Provide the server side as a `PromptHandler` trait; the desktop (stream K) implements it.
   Never log prompts' answers.
4. **Machine probe.** One ssh call runs a POSIX-sh script that reports, as simple `key=value`
   lines:
   - OS and architecture;
   - `$HOME`;
   - whether `tmux` exists, with `tmux -V`;
   - whether `sbatch` and `squeue` exist;
   - the filesystem type of `$HOME` (`stat -f -c %T` on Linux, `df -T` fallback), for the
     network-FS flag.

   Parse the result into `MachineInfo`. Tolerate missing tools and odd output.

## Acceptance

- **Fake-ssh tests** (a test binary or script whose behaviour is driven by env or a scenario
  file) cover:
  - the exact argument list (options present, `--` before the host);
  - quoting of hostile argv: `'`, `;`, `$(…)`, backticks, newlines, `-rf`, non-ASCII;
  - an askpass round trip, including cancel;
  - a host-key prompt;
  - connect timeout and non-zero exit mapping;
  - probe parsing for Linux, macOS, a SLURM login node and a box without tmux.
- **Property test:** for random argv, unquoting the produced command with a POSIX-sh lexer model
  gives back the argv exactly.
- An optional integration test against a real `sshd`, skipped unless an env var points at one.

## Out of scope

Helper upload and verification, launchers (direct, tmux, systemd, SLURM), tunnels and stdio
bridge, reconnect ladder, WSL (later briefs).
