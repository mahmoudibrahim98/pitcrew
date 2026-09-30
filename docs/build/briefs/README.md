# Briefs

A brief is one agent's assignment: one stream, one branch, one goal. To start an agent, open a
Claude Code session in this repository (or in a worktree of it) and say:

> Follow `docs/build/briefs/<brief>.md`.

| Brief | Stream | Branch | Suggested model | Status |
|---|---|---|---|---|
| [A-claude-adapter](A-claude-adapter.md) | A · Ingest | `s/A/claude-adapter` | Opus-class | Ready |
| [B-control-mode-and-buffer](B-control-mode-and-buffer.md) | B · Runtime | `s/B/control-mode-and-buffer` | Opus-class | Ready |
| [C-open-and-log](C-open-and-log.md) | C · Store | `s/C/open-and-log` | Sonnet-class | Ready |
| [H-listener-and-tokens](H-listener-and-tokens.md) | H · API and auth | `s/H/listener-and-tokens` | Opus-class | Ready |
| [L-skeleton-and-data](L-skeleton-and-data.md) | L · UI foundation | `s/L/skeleton-and-data` | Opus-class | Ready |

The integrator adds new briefs here as streams progress, and marks them done when merged.

---

Everything below applies to **every** brief. Read it before starting.

## 1. Set up your workspace

You work in **your own git worktree on your brief's branch**, never directly in the integrator's
checkout of `main`.

1. Run `git rev-parse --show-toplevel` and `git branch --show-current`.
2. **Already on your brief's branch?** A previous run started it. Read `git log main..HEAD` and
   continue from there.
3. **In a worktree created for you** (the branch is neither `main` nor yours): rename the branch
   with `git branch -m <your branch>`. Check it starts from `main`:
   `git merge-base --is-ancestor main HEAD`.
4. **In the integrator's checkout, on `main`:** create your worktree next to it and work only
   there:

   ```bash
   git worktree add ../pitcrew-wt/<stream letter> -b <your branch> main
   ```

   Then run every command from inside that folder and edit only files under it. Never edit files
   in the integrator's checkout.

## 2. Environment

- **Windows:** use PowerShell. Run git from PowerShell, not inside WSL, because the worktree
  metadata uses Windows paths. Run Rust inside WSL, with **your own target directory** so parallel
  agents don't block each other:

  ```bash
  wsl.exe -d Ubuntu-22.04 --cd "$PWD" -- bash -lc 'CARGO_TARGET_DIR=$HOME/.cache/pitcrew-target-<stream letter> cargo test --workspace'
  ```

  The same form works for `cargo fmt --all`, `cargo clippy …` and `cargo test -p <crate>`.
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
