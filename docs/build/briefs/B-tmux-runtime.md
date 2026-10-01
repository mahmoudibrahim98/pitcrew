# Brief B · `TmuxRuntime`: real terminals on tmux

- **Stream:** B · Runtime. **Branch:** `s/B/tmux-runtime`. **Paths:** `crates/runtime/**`
  (+ `Cargo.lock`).
- **First read:** [README.md](README.md), `docs/build/streams/B.md` (work packages 1, 2, 3 and 6),
  [B-control-mode-and-buffer.md](B-control-mode-and-buffer.md) and
  [B-control-hardening.md](B-control-hardening.md) (both merged), the `crates/runtime` README and
  sources (the control-mode parser, the command builder, `ReplayBuffer`, detection),
  `crates/interfaces/src/runtime.rs` (the `Runtime` trait and its contract), `Key` in
  `crates/protocol/src/runner.rs`, ADR-0005, ADR-0010, and the `crates/runner` README
  (`RunnerTerminals`: how the runner uses a `Runtime`).

## Goal

Agent sessions run in real terminals that PitCrew can start, watch, type into and resize. They
survive a daemon restart, and a person can `tmux attach` to them by hand. On a machine without
tmux, the runtime says so, and the PTY runtime comes in the next brief.

## What to build

1. **A long-lived control client** (work package 1): one `tmux -C` connection per tmux server,
   using the merged parser and builder.
   - Commands go over the connection, without forking per command.
   - Replies are matched to their commands.
   - `%output` goes into the right terminal's `ReplayBuffer`.
   - Reconnect if the client dies, without losing a terminal's offsets.
   - A dedicated tmux server (socket) for PitCrew, private to the user (a 0700 directory), so a
     person's own tmux sessions are never touched. Document how to `tmux -S <socket> attach`.
2. **`TmuxRuntime` implementing `Runtime`** (work package 2), exactly to the trait's contract:
   - `start`: a new window running the command with its arguments (argv, never a shell string)
     in the given folder, with environment variables, and tagged with a PitCrew window option
     (`@pitcrew-terminal=<id>`).
   - `write` (bytes, `send-keys -H`) and `send_keys` (`Key` to tmux key names).
   - `resize`.
   - `screen()` from a `vt100` model fed by `%output`, so a prompt drawn with cursor movement is
     correct.
   - `read_output(from)`, with `truncated`.
   - `info`, `list` (finds tagged windows after a restart, and output resumes from the last
     offset), and `kill`.
   - `native_target` for `tmux attach`.
   - Bound every call in time.
3. **Detection** (work package 6, tmux half): tmux present and usable (version, can start a
   server on the private socket), giving `Capability::Tmux`. Otherwise the runtime is
   unavailable with a reason. Detection doesn't block an async caller.

## Acceptance

- **Integration tests on tmux 3.2a** (installed in the dev WSL; skipped where tmux is missing):
  - `sh -c 'printf ready; cat'`: write, read back by offset, resize, `ctrl_c`, kill;
  - a restart: start, drop the runtime, create a new one, `list()` finds the terminal and output
    resumes from the last offset;
  - `screen()` with a prompt drawn by cursor movement;
  - two terminals interleaving output;
  - a reconnect after killing the control client;
  - a hostile command name or argument (quotes, `;`, newlines, `#{...}`) that runs literally.
- **Throughput:** 50 MB of output streamed, with CPU use reported. The goal is under 5% of one
  core. Report it, don't block on it.
- **Leaks:** the test tmux servers and their processes are gone after the tests. Reuse
  `crates/remote`'s leak-guard idea.
- fmt, clippy (Linux and Windows target; tmux code may be `cfg(unix)`), `cargo test --workspace`,
  `cargo deny check`, and the guards all pass.

## Out of scope

The PTY runtime and `pitcrew-ptyd` (the next brief), and wiring it into the daemon (stream 0).
