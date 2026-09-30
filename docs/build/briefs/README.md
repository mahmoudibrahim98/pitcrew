# Briefs

A brief is one agent's assignment: one stream, one branch, one goal. The integrator creates a
git worktree on the brief's branch, opens a Claude Code session in it, and says:

> Follow `docs/build/briefs/<brief>.md`.

| Brief | Stream | Branch | Suggested model | Status |
|---|---|---|---|---|
| [A-claude-adapter](A-claude-adapter.md) | A · Ingest | `s/A/claude-adapter` | Opus-class | **Merged** |
| [A-codex-adapter](A-codex-adapter.md) | A · Ingest | `s/A/codex-adapter` | Opus-class | **Merged** |
| [A-opencode-adapter](A-opencode-adapter.md) | A · Ingest | `s/A/opencode-adapter` | Opus-class | In progress |
| [B-control-mode-and-buffer](B-control-mode-and-buffer.md) | B · Runtime | `s/B/control-mode-and-buffer` | Opus-class | **Merged** |
| [B-control-hardening](B-control-hardening.md) | B · Runtime | `s/B/control-hardening` | Opus-class | In review: fixes requested |
| [C-open-and-log](C-open-and-log.md) | C · Store | `s/C/open-and-log` | Sonnet-class | **Merged** |
| [C-store-hardening](C-store-hardening.md) | C · Store | `s/C/store-hardening` | Sonnet-class | **Merged** |
| [C-projections](C-projections.md) | C · Store | `s/C/projections` | Sonnet-class | In review: small fixes |
| [E-work-core](E-work-core.md) | E · Work model | `s/E/work-core` | Opus-class | Ready (based on C-projections) |
| [D-watch-and-index](D-watch-and-index.md) | D · Runner | `s/D/watch-and-index` | Opus-class | In review: fixes in progress |
| [H-listener-and-tokens](H-listener-and-tokens.md) | H · API and auth | `s/H/listener-and-tokens` | Opus-class | **Merged** |
| [H-delta-stream](H-delta-stream.md) | H · API and auth | `s/H/delta-stream` | Opus-class | **Merged** |
| [H-terminal-and-activity](H-terminal-and-activity.md) | H · API and auth | `s/H/terminal-and-activity` | Opus-class | In review |
| [I-cli-and-hooks](I-cli-and-hooks.md) | I · CLI and hooks | `s/I/cli-and-hooks` | Sonnet-class | In progress |
| [J-ssh-connection](J-ssh-connection.md) | J · Remote and HPC | `s/J/ssh-connection` | Opus-class | In review: fixes in progress |
| [L-skeleton-and-data](L-skeleton-and-data.md) | L · UI foundation | `s/L/skeleton-and-data` | Opus-class | In review: round 3 |
| [L-shell](L-shell.md) | L · UI foundation | `s/L/shell` | Opus-class | In progress (based on skeleton-and-data) |
| [M-console-components](M-console-components.md) | M · Agent console | `s/M/console-components` | Opus-class | In progress (based on skeleton-and-data) |
| [N-projects-components](N-projects-components.md) | N · Projects layout | `s/N/projects-components` | Sonnet-class | In progress (based on skeleton-and-data) |
| [F-activity-blocks](F-activity-blocks.md) | F · Recap | `s/F/activity-blocks` | Opus-class | In progress |
| [P-bench-and-release](P-bench-and-release.md) | P · Packaging | `s/P/bench-and-release` | Sonnet-class | In progress |
| [Q-threat-model-and-fuzz](Q-threat-model-and-fuzz.md) | Q · Security | `s/Q/threat-model-and-fuzz` | Opus-class | In progress |

The branch name matters: CI's path guard reads the stream from it (`s/<stream>/<topic>`). The
integrator adds new briefs here as streams progress, and marks them done when merged.

---

Everything below applies to **every** brief. Read it before starting.

## 1. Check your workspace

The integrator has already created your worktree on your brief's branch. **Do not create
worktrees or branches, and do not switch branches.** Before any other work:

1. `git branch --show-current` must print exactly the branch named in your brief.
2. If `git log main..HEAD` shows commits, a previous run started this work: read them and
   continue from there.
3. `main` may have moved on since your branch was created. That is expected: **do not rebase
   onto or merge `main`**. The integrator resolves that when merging your branch.

If the branch is wrong, or you are on `main`, **stop and tell the user**. Edit only files inside
this worktree.

## 2. Environment

- **Windows:** use PowerShell. Run git from PowerShell, not inside WSL, because the worktree
  metadata uses Windows paths. Run Rust inside WSL, with **your own target directory** so parallel
  agents don't block each other:

  ```bash
  wsl.exe -d Ubuntu-22.04 --cd "$PWD" -- bash -lc 'CARGO_BUILD_JOBS=4 CARGO_TARGET_DIR=$HOME/.cache/pitcrew-target-<stream letter> cargo test --workspace'
  ```

  The same form works for `cargo fmt --all`, `cargo clippy …` and `cargo test -p <crate>`.
  Keep `CARGO_BUILD_JOBS=4`: several agents build at once, and unbounded parallel builds have
  run the machine out of memory.
- **Linux and macOS:** run `cargo` directly.
- **Node 24** runs natively. `npm test` runs the mock-hub and CI-script tests.
  `npm run mock-hub` serves the fake daemon on `http://127.0.0.1:47317`. Its tokens are
  `dev-device-token` (a person) and `dev-agent-token` (an agent).
- **pnpm** runs through corepack: `corepack pnpm <command>`. The version is pinned in the root
  `package.json`.

## 3. Rules

- **Only edit your stream's paths** (`docs/build/ownership.json`). If you need a change elsewhere
  (the protocol, the contracts, the root `Cargo.toml`, CI, another stream's crate), **stop and
  describe it in your report**. If it blocks you, write a local stand-in inside your own paths.
- **Build against contracts, not other streams' code:**
  - the protocol types (`crates/protocol`);
  - the traits and their fakes (`crates/interfaces`);
  - the fixtures (`crates/fixtures`);
  - the API contract (`docs/build/contracts/api-v1.md`) and the mock hub (`apps/mock-hub`).
- **Dependencies:** prefer `name.workspace = true` from the root `Cargo.toml`. For anything else,
  add an exact version to your own crate's `Cargo.toml` and say why in your report. The licence
  must be allowed by `deny.toml`.
- **Nothing private:** synthetic test data only. No real transcripts, host names, user names,
  personal paths or tokens. Real samples for local checks go in a `private/` folder inside your
  crate; it is git-ignored.
- **Quality:** no `unsafe` unless your brief allows it, and then isolated in one module with a
  comment saying why it is sound. No `unwrap` outside tests. Comments are sparse, plain and useful.
- **Stay in scope.** Do your brief's goal, not the whole card. List anything extra you noticed in
  your report.

## 4. Done means

All of these pass, run from your worktree:

```bash
cargo fmt --all --check
cargo clippy --workspace --all-targets -- -D warnings
cargo test --workspace
npm test
node scripts/ci/path-guard.mjs --base main
node scripts/ci/ownership-audit.mjs --untracked
```

Plus every acceptance check in your brief.

## 5. Finish

1. Commit to your branch in small, clear commits. End each message with the co-author line your
   tool adds, if any.
2. **Do not push, merge, rebase `main`, or touch any other branch.**
3. End with a report in the shape of `.github/pull_request_template.md`:
   - what changed, with the files;
   - how you checked it, pasting **real output**;
   - contract changes you need (exact types and routes);
   - **"What I did not do"**: anything skipped, stubbed, unverified or left for later.
