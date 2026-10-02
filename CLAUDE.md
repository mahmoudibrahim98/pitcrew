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

## Several briefs in one cloud session (main session plus subagents)

When one cloud session is asked to run several briefs, it is the **main session**. Each brief runs as
a **subagent**, and all of them share one VM. The main session:

1. **Gives each brief a worktree and branch.** For each brief, from an up-to-date `main`:
   `git worktree add ../wt-<short-name> -b <the brief's branch> origin/main`. A subagent works only in
   its own worktree.
2. **Gives each worktree its own build cache:**
   `CARGO_TARGET_DIR=/root/.cache/pitcrew-target-<short-name>`, outside the worktree. **Never share one
   between worktrees.** Cargo can then reuse one worktree's build of a workspace crate for another
   (tests run the other branch's code), and crates that embed absolute paths (`pitcrew-store`,
   `pitcrew-fixtures`) break when that worktree is removed.
3. **Runs at most two subagents that build Rust at a time.** Briefs that only touch docs, Node or CI
   files may run alongside. Check `df -h` before starting another, and keep at least 6 GB free.
   - With the environment's settings (no debug info, no incremental data), one cache takes roughly
     8–12 GB.
   - When a subagent's pull request is open, delete its cache (`rm -rf
     /root/.cache/pitcrew-target-<short-name>`) and remove its worktree, before starting the next
     brief.
4. **Tells each subagent:**
   - to read this file, `docs/build/briefs/README.md` and its brief;
   - to commit to its branch in its worktree;
   - to push and open its own pull request with its report as the body;
   - to stay in its brief's paths;
   - to touch no other worktree.
5. **Doesn't merge anything.** The integrator reviews and merges every pull request. The main session
   reports the pull-request URLs, and what each subagent said it did not do.
6. **Cleans up** each worktree (`git worktree remove`) and its build cache after its pull request is
   open, so the next brief has room.
