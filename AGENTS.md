# PitCrew: instructions for coding agents

An open-source desktop app (Rust core `pitcrewd`, Tauri 2 shell, React 19 UI) that tracks AI coding
agents' sessions locally and on remote and HPC machines. The work is split into **briefs**:
`docs/build/briefs/`. This file is for agents that read `AGENTS.md`, such as Codex. `CLAUDE.md` holds
the same rules for Claude Code sessions.

**Read `docs/build/briefs/README.md` before any work.** Its rules apply to every task: the branch,
your stream's paths, contracts first, nothing private, and what "done" means. Then read the brief you
were given, and the READMEs of the crates or apps it names.

## The branch decides what you may change

CI's path guard (`scripts/ci/path-guard.mjs`) reads the pull request's branch name:

- `s/<stream>/<topic>`: only paths that stream owns (`docs/build/ownership.json`), plus shared files;
- `integrator/<topic>`: any path, for work across streams.

Any other branch name fails CI. **Use exactly the branch your brief names.** If your tool picks the
branch name itself (e.g. `codex/...`), say so at the top of the pull request's body, and the
integrator re-pushes it under the brief's name.

Never push to `main`, and never merge. The integrator reviews and merges every pull request.

## Building and testing

- **Toolchain:**
  - Rust 1.88 or newer (the workspace's `rust-version`);
  - Node 22.18 or newer, with pnpm through corepack (`corepack pnpm install --frozen-lockfile`;
    the version is pinned in the root `package.json`);
  - tmux, for the runtime's tests;
  - on Linux, the WebKitGTK development libraries, for the desktop crate.
- **Disk can run out.** Set these if the environment doesn't:
  - `CARGO_PROFILE_DEV_DEBUG=0`
  - `CARGO_PROFILE_TEST_DEBUG=0`
  - `CARGO_INCREMENTAL=0`

  Use the default `target/` only: no release or fuzz builds unless your brief asks.
- **Test what you touched first:** `cargo test -p <crate> --no-fail-fast` for each crate you changed,
  and `npm test` / `npm run typecheck` / `npm run lint` in `apps/ui` for UI work. A full
  `cargo test --workspace` takes 10–20 minutes; run it if you have the time and disk, and say in
  your report whether you did.
- **Before you finish:**
  - `cargo fmt --all --check`;
  - `cargo clippy --workspace --all-targets --locked -- -D warnings`, or `-p` for each crate you
    changed if the workspace is too big for the environment;
  - the guards: `node scripts/ci/ownership-audit.mjs`,
    `node scripts/ci/path-guard.mjs --base origin/main`, and
    `node scripts/ci/scrub-gate.mjs --base origin/main`.
- **Windows and macOS** are checked by CI on your pull request, not locally. Code behind
  `#[cfg(windows)]` or `#[cfg(target_os = "macos")]` must still compile and pass clippy there: read
  CI's results and fix what fails.
- **No network after setup?** If a dependency can't be fetched, stop and say so in your report.
  Don't vendor crates, add registries, or work around the lockfile.

## Rules

- **Stay in your brief's paths.** If the brief turns out to need a file outside them, stop and
  explain it in the report rather than editing it.
- **Contracts first.** If your change alters an interface between streams (`docs/build/contracts/`),
  change the contract in the same pull request, before the code, and say so.
- **Don't weaken what guards the code:**
  - no deleted or loosened assertions;
  - no new `#[ignore]`, `allow(...)` or `// eslint-disable` to get past a failure;
  - no edits to the guards, `ownership.json`, `deny.toml` or CI workflows unless your brief says
    so.

  If a test is wrong, fix it and explain why in the report.
- **Privacy:** synthetic data only (`/home/sam/...`, `example.com`, "a SLURM cluster"). Never put real
  host names, user names, paths or e-mail addresses in files, tests, commits or the pull request.
  The scrub gate runs in CI with the maintainers' private patterns, so a leak fails the pull
  request; don't try to work around it. Never print or commit tokens.
- **Agent homes:** tests always use temporary homes (`pitcrew_fixtures::homes`). Never point anything
  at a real `~/.claude`, `~/.codex` or OpenCode data folder, and never run a real `claude`, `codex` or
  `opencode` binary in a test: use the stand-ins the tests already have.

## The pull request

Push your branch and open a pull request whose body is your report, in the shape of
`.github/pull_request_template.md`. The report must cover:
- what you changed, and why;
- what you ran, with the results;
- what you could not run, or chose not to do.

CI must pass. If it fails on Windows or macOS only, fix it from CI's log.
