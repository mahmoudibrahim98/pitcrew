# Brief B · `pitcrew-ptyd` and `PtyRuntime`: terminals without tmux, Windows included

- **Stream:** B · Runtime. **Branch:** `s/B/pty-runtime`. **Paths:** `crates/runtime/**`,
  `crates/ptyd/**` (+ `Cargo.lock`, and the root `Cargo.toml`'s member list for the new crate).
- **First read:** [README.md](README.md), `docs/build/streams/B.md` (work packages 4, 5 and 6),
  the `crates/runtime` README (all of it, including both review rounds of `TmuxRuntime`: the
  vt100 clamp filter, per-terminal locks, the work budget, kills by process group, untrusted
  input), `crates/interfaces/src/runtime.rs`, ADR-0005, ADR-0010, and `docs/security/threat-model.md`
  (the runtime rows).

## Goal

On a machine without tmux (native Windows above all, where most people will run the desktop),
PitCrew still runs agents in real terminals. A small supervisor, `pitcrew-ptyd`, owns them and
outlives `pitcrewd`, so upgrading or restarting the daemon never kills a session.

## What to build

1. **`pitcrew-ptyd`** (`crates/ptyd`), a small, rarely changing binary.
   - It owns PTYs through `portable-pty` (ConPTY on Windows), one per terminal, each with a
     bounded replay buffer and a vt100 screen model. Reuse the runtime's clamp filter, per-terminal
     locks and work budget, so a hostile program's output can't freeze or exhaust it.
   - It serves a small, versioned protocol (its own version, separate from API v1):
     - start (argv, never a shell string; cwd; env);
     - write;
     - keys;
     - resize;
     - screen;
     - read output from an offset;
     - list;
     - kill;
     - info.
   - **Where it listens:**
     - Unix: a socket in a 0700 directory (the same rules as the tmux socket: private
       `$XDG_RUNTIME_DIR`, else `/tmp/pitcrew-<uid>`, checked before every use). The peer's uid
       must be ours.
     - Windows: a named pipe whose DACL allows only the current user. Check the client's user
       SID too (the pipe's client process token). Refuse remote clients
       (`PIPE_REJECT_REMOTE_CLIENTS`).
   - **One per user.** A second instance refuses to start (a lock). It exits on its own only when
     it has no terminals and no client for a while.
   - **Kill** ends the program's whole tree:
     - Unix: the PTY child leads its session, so signal the group, TERM then KILL;
     - Windows: every terminal's process in a Job Object, closed on kill.
   - Any `unsafe` (Windows APIs) sits in one module with a soundness comment per block.
2. **`PtyRuntime`** implementing `Runtime` as a client of `pitcrew-ptyd`.
   - It starts ptyd if none is running, detached so it outlives the caller.
   - It reconnects without losing offsets.
   - Every call is bounded in time.
   - After a restart, `list()` finds the terminals and output resumes from the last offset.
   - `native_target` says how to reach a terminal by hand, if anything can (or `None`).
3. **Detection:** tmux when present and usable, else PTY. Report `Capability::Tmux` or
   `Capability::Pty`. Detection never blocks an async caller.
4. **The binary's name and place:** `pitcrew-ptyd` is found next to `pitcrewd` (as the desktop
   finds its helpers), never on `PATH`. Say what stream P must bundle.

## Acceptance

- **Integration tests on Linux** (PTY, always available): `sh -c 'printf ready; cat'`:
  - write, read back by offset, resize, `ctrl_c` and kill;
  - a restart, where the runtime is dropped, a new one finds the terminal, and output resumes;
  - `screen()` with a prompt drawn by cursor movement;
  - two terminals interleaving;
  - a reconnect after killing the client side;
  - ptyd outliving its client;
  - hostile output (the clamp cases from `TmuxRuntime`'s tests) not freezing another terminal;
  - a second ptyd refused;
  - a peer from another uid refused (or, if a second uid isn't available, the check unit-tested).
- **Windows:**
  - the same tests compile for the Windows target and run on CI's `windows-latest` (`cmd /c`
    in place of `sh -c`);
  - native Windows test runs aren't possible on the dev machine, so say what you could only
    compile;
  - the pipe's DACL and the client-SID check are unit-tested where possible.
- **Leaks:** no ptyd, PTY or child left after the tests.
- **Throughput:** 50 MB streamed, with CPU reported, in a debug build. No release builds:
  disk is tight.
- fmt, clippy (Linux and Windows target), `cargo test --workspace`, `cargo deny check`, and the
  guards all pass.

## Out of scope

Wiring into the daemon (stream 0, after `0-daemon-runtime`), bundling (stream P), and the
desktop.
