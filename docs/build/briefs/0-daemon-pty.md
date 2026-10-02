# Brief 0 · The daemon's terminals without tmux: `PtyRuntime`

- **Stream:** 0 · Composition root (integration work). **Branch:** `integrator/daemon-pty`.
  **Paths:** `crates/daemon/**` (+ `Cargo.lock`). Leave the daemon's tmux tests to the
  `B-tmux-compat` session, which is changing them.
- **First read:** [README.md](README.md), the root `CLAUDE.md`, and the READMEs of `crates/daemon`
  (terminals, the tmux socket per state directory, stop), `crates/runtime` (`PtyRuntime`, `choose`,
  `choose_async`, `Chosen`, `PtyOptions`) and `crates/ptyd` (what it needs, bundling notes, deployment
  hazards).

## Goal

On a machine without a usable tmux (native Windows above all), sessions PitCrew starts run in
terminals owned by `pitcrew-ptyd`, which outlives the daemon. Today the daemon falls back to
`NoRuntime` there, so no session can start in a terminal.

## What to build

1. **Choose the runtime:** replace the tmux-only detection with
   `choose_async(TmuxOptions, PtyOptions)`.
   - tmux when it is usable, else the PTY runtime.
   - Report `chosen.capability()` (`tmux` or `pty`) in host info.
   - Build the runtime with `Chosen::into_runtime`, behind the existing `Detachable` wrapper.
   - Every call stays on the blocking pool, with the same bounds as today.
2. **Find `pitcrew-ptyd`** next to the running `pitcrewd` (same directory, `pitcrew-ptyd.exe` on
   Windows), never on `PATH`. A missing binary means no PTY runtime, with a warning saying where it
   looked.
3. **Per state directory:** the ptyd endpoint follows the same rule as the tmux socket: one per state
   directory, so a demo daemon never reaches the real daemon's terminals. Hold the same lock.
4. **Test overrides:** a hidden option for the ptyd endpoint and binary (like `--tmux-socket`), and one
   that forces the PTY runtime even where tmux exists (e.g. `--terminal-runtime pty`), both with a
   warning when used.
5. **Stop:** drop the runtime after the runner stops. ptyd and its terminals keep running, and offsets
   live in ptyd.
6. **README:** document the choice, the binary's place, the per-state-directory endpoint, and the two
   deployment hazards from ptyd's README:
   - a systemd unit's default `KillMode=control-group` kills ptyd too;
   - any Job Object around `pitcrewd` must allow breakaway.

## Acceptance

- **Tests, forcing the PTY runtime** (they run on Linux in the VM and natively on CI's Windows):
  - `POST /v1/sessions` starts a terminal running a stand-in CLI;
  - the terminal WebSocket streams its output;
  - `send`, `keys`, `interrupt` and `end` reach it;
  - host info says `pty`;
  - after a daemon restart the terminal is found again and output resumes from its offset;
  - two daemons on two state directories get two ptyd endpoints;
  - a missing `pitcrew-ptyd` gives no runtime and a clear log line;
  - no ptyd or child is left behind.
- The tmux path still works where tmux is usable (the existing tests).
- fmt, clippy, `cargo test --workspace` (in the background; mind the VM's disk), `npm test`, and the
  guards pass. CI passes on all three systems, apart from failures `0-ci-green` and `B-tmux-compat`
  are fixing.

## Out of scope

Bundling `pitcrew-ptyd` into installers (`P-desktop-bundle`), the dispatcher, and the desktop app.
