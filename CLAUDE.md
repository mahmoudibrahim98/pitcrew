# PitCrew

An open-source desktop app (Rust core `pitcrewd`, Tauri 2 shell, React 19 UI) that tracks AI coding
agents' sessions locally and on remote and HPC machines. The plan and the work are split into
**briefs**: `docs/build/briefs/`.

**Read `docs/build/briefs/README.md` before any work.** Its rules apply to every session: the
branch, your stream's paths, contracts first, nothing private, and what "done" means. Then read
the brief you were given.

## Cloud sessions

When `CLAUDE_CODE_REMOTE` is `true`, you run in a cloud VM (Ubuntu 24.04, about 4 CPUs, 16 GB of
RAM and **30 GB of disk**).

- **Disk is the limit.** The workspace's build directory can outgrow the disk.
  - The environment sets `CARGO_PROFILE_DEV_DEBUG=0`, `CARGO_PROFILE_TEST_DEBUG=0` and
    `CARGO_INCREMENTAL=0`; keep them.
  - Use the default `target/` only: no second target directory, no release or fuzz builds unless
    your brief asks.
  - Check `df -h` before a full workspace build.
- **Run cargo directly.** tmux is installed, so the tmux tests run. Windows and macOS are checked by
  CI on your pull request, not in the VM: don't cross-compile for Windows.
- **Long runs:** a full `cargo test --workspace` takes 10–20 minutes. Run it in the background and
  read its log, rather than waiting on a foreground command that times out.
- **Branch and pull request:**
  - Work on the branch your brief names (`s/<stream>/<topic>`); create it from `main` if the session
    started elsewhere.
  - Never push to `main`.
  - When done, push the branch and open a pull request whose body is your report, in the shape of
    `.github/pull_request_template.md`.
  - CI must pass. The integrator reviews and merges.
- **Privacy:** synthetic data only. The scrub gate (`scripts/ci/scrub-gate.mjs`) runs in CI with the
  maintainers' private patterns, so a leak fails the pull request; don't try to work around it.
  Never print or commit tokens.
- **Agent homes:** tests always use temporary homes. Never point anything at a real `~/.claude`,
  `~/.codex` or OpenCode data folder, in the cloud or anywhere else.
