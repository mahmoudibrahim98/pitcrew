# Brief 0 · Upgrade `getrandom` to 0.4

- **Stream:** 0 · Contracts (one dependency used by auth, remote and the desktop).
  **Branch:** `integrator/deps-getrandom-04`.
  **Paths:**
  - `crates/auth/Cargo.toml` and `crates/auth/src/token.rs`;
  - `crates/remote/Cargo.toml` and `crates/remote/src/askpass/mod.rs`;
  - `apps/desktop/src-tauri/Cargo.toml` and `apps/desktop/src-tauri/src/remote/mod.rs`;
  - `Cargo.lock` and `apps/desktop/src-tauri/Cargo.lock`.
- **First read:** [README.md](README.md), the root `AGENTS.md` (or `CLAUDE.md`), and `getrandom`'s
  changelog for 0.4.
- **Suggested agent:** Codex, or any coding agent.

## Goal

Dependabot's PR #2 bumps `getrandom` from 0.3.4 to 0.4.3. `getrandom` makes the secrets PitCrew
relies on:
- API tokens (`crates/auth/src/token.rs` ~50);
- the askpass helper's one-time secret (`crates/remote/src/askpass/mod.rs` ~330);
- the desktop's remote secret (`apps/desktop/src-tauri/src/remote/mod.rs` ~1949).

Move all three to 0.4 and keep them on the OS's secure random source.

## What to do

1. **Check the minimum Rust version first.** If 0.4 needs a newer Rust than the workspace's
   `rust-version` (1.88), stop and report it; don't raise `rust-version` in this brief.
2. **Keep the exact pins:** `crates/auth` and `crates/remote` pin `getrandom = "=0.3.4"` on purpose,
   so pin `=0.4.3` the same way. The desktop's `"0.3"` becomes `"0.4"`.
3. Fix the three call sites for any API change (`getrandom::fill` and its error type).
   - A failure must still be an error the caller handles, as today: no `unwrap`, and no fallback to
     a weaker source.
   - Say in the report which backend 0.4 uses on Linux, macOS and Windows, and whether any of them
     needs a feature flag or `cfg`. If one does, set it in the manifests.
4. **Other crates may still pull `getrandom` 0.2 or 0.3** (`rand`, `tempfile`, `uuid`...). That is
   expected: list them from `cargo tree -i getrandom`. `deny.toml` warns on duplicate versions;
   don't change it.

## Acceptance

- `cargo test -p pitcrew-auth -p pitcrew-remote` passes, and `cargo test` in `apps/desktop/src-tauri`
  passes if the environment can build it (WebKitGTK); say if it couldn't.
- fmt, clippy with `-D warnings`, and the guards pass. Every CI job passes on the pull request,
  Windows and macOS included.

## Out of scope

Other dependency upgrades, and changing how many random bytes each secret uses.
