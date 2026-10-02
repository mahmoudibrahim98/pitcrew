# Brief B · TmuxRuntime on tmux 3.3, 3.4 and 3.5

- **Stream:** B · Runtime. **Branch:** `s/B/tmux-compat`. **Paths:** `crates/runtime/**`, and the tmux
  tests in `crates/daemon/tests/` (+ `Cargo.lock`).
- **First read:** [README.md](README.md), the root `CLAUDE.md`, and the `crates/runtime` README
  (`TmuxRuntime`, its control client, both review rounds, the tmux 3.2a findings).

## Goal

`TmuxRuntime` was built and tested on tmux 3.2a. On tmux 3.4 (Ubuntu 24.04, and GitHub's Linux
runners) its tests fail with "the tmux control connection closed", in `crates/runtime` and in the
daemon's tmux tests. Make it work on tmux 3.3, 3.4 and 3.5, and keep 3.2a working.

## What to build

1. **Find out why the control connection closes on 3.4.** The cloud VM has tmux 3.4. Read tmux's
   CHANGES between 3.2a and 3.5 for control-mode changes:
   - how `-C` clients attach;
   - `%exit` and its reasons;
   - `refresh-client` sizes;
   - `new-session` and `attach` behaviour;
   - `window-size`;
   - flags in `%begin`.

   Reproduce it on a private socket (`tmux -S <dir>/s …`).
2. **Fix it** in the control client and runtime, keeping every guarantee from the review rounds:
   - the clamp filter and per-terminal locks;
   - guard-flag reply matching;
   - the orphan sweep;
   - the socket directory checks;
   - kills by process group;
   - tag rules.

   Where versions need different behaviour, decide by the detected version, and document it.
3. **Detection** reports the versions supported. Keep 3.2 as the floor unless a reason appears, and
   say so in the README.
4. **CI and the VM:** the runtime's and the daemon's tmux tests pass on 3.4 (in the VM, and on
   CI's ubuntu-latest). If you can build tmux 3.5 from source in the VM within the disk limit, run the
   tests on it too; otherwise say so. 3.2a is checked by the integrator locally; don't break it.
   Note any change that might affect 3.2a in your report.

## Acceptance

- `cargo test -p pitcrew-runtime` and the daemon's tmux tests pass on tmux 3.4 in the VM, five runs
  in a row.
- No processes left behind (the tests' marker and sweep).
- fmt, clippy, `cargo test --workspace` (mind the VM's disk), and the guards pass. CI's Linux job
  passes its tmux tests on the pull request.

## Out of scope

The PTY runtime, and the daemon beyond its tmux tests.
