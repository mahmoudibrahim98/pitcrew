# Briefs

A brief is one agent's assignment: one stream, one branch, one goal. The integrator creates a
git worktree on the brief's branch, opens a Claude Code session in it, and says:

> Follow `docs/build/briefs/<brief>.md`.

| Brief | Stream | Branch | Suggested model | Status |
|---|---|---|---|---|
| [A-claude-adapter](A-claude-adapter.md) | A · Ingest | `s/A/claude-adapter` | Opus-class | **Merged** |
| [A-codex-adapter](A-codex-adapter.md) | A · Ingest | `s/A/codex-adapter` | Opus-class | **Merged** |
| [A-opencode-adapter](A-opencode-adapter.md) | A · Ingest | `s/A/opencode-adapter` | Opus-class | **Merged** |
| [B-control-mode-and-buffer](B-control-mode-and-buffer.md) | B · Runtime | `s/B/control-mode-and-buffer` | Opus-class | **Merged** |
| [B-control-hardening](B-control-hardening.md) | B · Runtime | `s/B/control-hardening` | Opus-class | In review: fixes requested |
| [C-open-and-log](C-open-and-log.md) | C · Store | `s/C/open-and-log` | Sonnet-class | **Merged** |
| [C-store-hardening](C-store-hardening.md) | C · Store | `s/C/store-hardening` | Sonnet-class | **Merged** |
| [C-projections](C-projections.md) | C · Store | `s/C/projections` | Sonnet-class | **Merged** |
| [E-work-core](E-work-core.md) | E · Work model | `s/E/work-core` | Opus-class | **Merged** |
| [D-watch-and-index](D-watch-and-index.md) | D · Runner | `s/D/watch-and-index` | Opus-class | **Merged** |
| [H-listener-and-tokens](H-listener-and-tokens.md) | H · API and auth | `s/H/listener-and-tokens` | Opus-class | **Merged** |
| [H-delta-stream](H-delta-stream.md) | H · API and auth | `s/H/delta-stream` | Opus-class | **Merged** |
| [H-terminal-and-activity](H-terminal-and-activity.md) | H · API and auth | `s/H/terminal-and-activity` | Opus-class | **Merged** |
| [H-terminal-hardening](H-terminal-hardening.md) | H · API and auth | `s/H/terminal-hardening` | Opus-class | **Merged** |
| [I-cli-and-hooks](I-cli-and-hooks.md) | I · CLI and hooks | `s/I/cli-and-hooks` | Sonnet-class | **Merged** |
| [J-ssh-connection](J-ssh-connection.md) | J · Remote and HPC | `s/J/ssh-connection` | Opus-class | **Merged** |
| [L-skeleton-and-data](L-skeleton-and-data.md) | L · UI foundation | `s/L/skeleton-and-data` | Opus-class | **Merged** |
| [L-shell](L-shell.md) | L · UI foundation | `s/L/shell` | Opus-class | **Merged** |
| [M-console-components](M-console-components.md) | M · Agent console | `s/M/console-components` | Opus-class | **Merged** |
| [N-projects-components](N-projects-components.md) | N · Projects layout | `s/N/projects-components` | Sonnet-class | **Merged** |
| [F-activity-blocks](F-activity-blocks.md) | F · Recap | `s/F/activity-blocks` | Opus-class | **Merged** |
| [P-bench-and-release](P-bench-and-release.md) | P · Packaging | `s/P/bench-and-release` | Sonnet-class | **Merged** |
| [Q-threat-model-and-fuzz](Q-threat-model-and-fuzz.md) | Q · Security | `s/Q/threat-model-and-fuzz` | Opus-class | **Merged** |

| [0-work-edits](0-work-edits.md) | 0 · Contracts | `integrator/work-edits` | Opus-class | **Merged** |
| [C-nfs-and-maintenance](C-nfs-and-maintenance.md) | C · Store | `s/C/nfs-and-maintenance` | Sonnet-class | **Merged** |
| [G-github-read](G-github-read.md) | G · Integrations | `s/G/github-read` | Sonnet-class | **Merged** |
| [E-single-writer-and-sessions](E-single-writer-and-sessions.md) | E · Work model | `s/E/single-writer-and-sessions` | Opus-class | **Merged** |
| [M-console-wiring](M-console-wiring.md) | M · Agent console | `s/M/console-wiring` | Opus-class | **Merged** |
| [M-terminal](M-terminal.md) | M · Agent console | `s/M/terminal` | Opus-class | **Merged** |
| [K-shell-and-gateway](K-shell-and-gateway.md) | K · Desktop shell | `s/K/shell-and-gateway` | Opus-class | **Merged** |
| [L-desktop-transport](L-desktop-transport.md) | L · UI foundation | `s/L/desktop-transport` | Opus-class | **Merged** |
| [G-jira-read](G-jira-read.md) | G · Integrations | `s/G/jira-read` | Sonnet-class | **Merged** |
| [Q-fuzz-and-model-refresh](Q-fuzz-and-model-refresh.md) | Q · Security | `s/Q/fuzz-and-model-refresh` | Opus-class | **Merged** |
| [0-recap-contract](0-recap-contract.md) | 0 · Contracts | `integrator/recap-contract` | Opus-class | **Merged** |
| [E-recap-index](E-recap-index.md) | E · Work model | `s/E/recap-index` | Opus-class | **Merged** |
| [H-recap-routes](H-recap-routes.md) | H · API and auth | `s/H/recap-routes` | Sonnet-class | **Merged** |
| [L-recap-data](L-recap-data.md) | L · UI foundation | `s/L/recap-data` | Sonnet-class | **Merged** |
| [C-import-and-reopen](C-import-and-reopen.md) | C · Store | `s/C/import-and-reopen` | Sonnet-class | **Merged** |
| [N-recap-views](N-recap-views.md) | N · Projects layout | `s/N/recap-views` | Opus-class | **Merged** |
| [K-tray-notifications-links](K-tray-notifications-links.md) | K · Desktop shell | `s/K/tray-notifications-links` | Opus-class | **Merged** |
| [J-tunnel](J-tunnel.md) | J · Remote and HPC | `s/J/tunnel` | Opus-class | In progress |
| [0-daemon-recaps](0-daemon-recaps.md) | 0 · Composition root | `integrator/daemon-recaps` | Opus-class | **Merged** |
| [F-recap-hardening](F-recap-hardening.md) | F · Recap | `s/F/recap-hardening` | Opus-class | **Merged** |
| [L-desktop-polish](L-desktop-polish.md) | L · UI foundation | `s/L/desktop-polish` | Sonnet-class | **Merged** |
| [M-session-work](M-session-work.md) | M · Agent console | `s/M/session-work` | Sonnet-class | **Merged** |
| [Q-round-3](Q-round-3.md) | Q · Security | `s/Q/round-3` | Opus-class | **Merged** |
| [D-hub-link-2](D-hub-link-2.md) | D · Runner | `s/D/hub-link-2` | Opus-class | **Merged** |
| [E-recap-names](E-recap-names.md) | E · Work model | `s/E/recap-names` | Sonnet-class | **Merged** |
| [I-hygiene](I-hygiene.md) | I · CLI and hooks | `s/I/hygiene` | Sonnet-class | **Merged** |
| [A-test-robustness](A-test-robustness.md) | A · Ingest | `s/A/test-robustness` | Sonnet-class | **Merged** |
| [G-fuzz-findings](G-fuzz-findings.md) | G · Integrations | `s/G/fuzz-findings` | Sonnet-class | **Merged** |
| [0-daemon-runner](0-daemon-runner.md) | 0 · Composition root | `integrator/daemon-runner` | Opus-class | In progress |
| [0-daemon-solo](0-daemon-solo.md) | 0 · Composition root | `integrator/daemon-solo` | Opus-class | **Merged** |
| [N-projects-wiring](N-projects-wiring.md) | N · Projects layout | `s/N/projects-wiring` | Sonnet-class | **Merged** |
| [L-shell-polish](L-shell-polish.md) | L · UI foundation | `s/L/shell-polish` | Sonnet-class | **Merged** |
| [E-edits-and-office](E-edits-and-office.md) | E · Work model | `s/E/edits-and-office` | Opus-class | **Merged** |
| [H-activity-index](H-activity-index.md) | H · API and auth | `s/H/activity-index` | Opus-class | **Merged** |
| [J-slurm](J-slurm.md) | J · Remote and HPC | `s/J/slurm` | Opus-class | **Merged** |
| [0-daemon-wiring](0-daemon-wiring.md) | 0 · Composition root | `integrator/daemon-wiring` | Opus-class | **Merged** |
| [A-scan](A-scan.md) | A · Ingest | `s/A/scan` | Sonnet-class | **Merged** |
| [D-hub-link](D-hub-link.md) | D · Runner | `s/D/hub-link` | Opus-class | In progress |
| [F-briefs-and-office](F-briefs-and-office.md) | F · Recap | `s/F/briefs-and-office` | Opus-class | **Merged** |
| [I-hook-install](I-hook-install.md) | I · CLI and hooks | `s/I/hook-install` | Sonnet-class | **Merged** |
| [J-deploy](J-deploy.md) | J · Remote and HPC | `s/J/deploy` | Opus-class | **Merged** |
| [O-onboarding-components](O-onboarding-components.md) | O · Onboarding | `s/O/onboarding-components` | Sonnet-class | **Merged** |
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
