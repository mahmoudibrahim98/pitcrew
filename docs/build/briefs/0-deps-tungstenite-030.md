# Brief 0 · Upgrade `tungstenite` to 0.30

- **Stream:** 0 · Contracts (one dependency used by the API, the runner's tests, the desktop and the
  fuzz targets). **Branch:** `integrator/deps-tungstenite-030`.
  **Paths:**
  - `crates/api/Cargo.toml`, `crates/api/src/**` and `crates/api/tests/**`;
  - `crates/runner/Cargo.toml` and `crates/runner/tests/terminals.rs`;
  - `apps/desktop/src-tauri/Cargo.toml` and `src/{gateway,attention,logging}*`, as needed;
  - `apps/desktop/src-tauri/tests/no_token.rs`;
  - `fuzz/Cargo.toml` and `fuzz/fuzz_targets/api_terminal.rs`;
  - the lockfiles.
- **First read:** [README.md](README.md), the root `AGENTS.md` (or `CLAUDE.md`), `crates/api`'s
  README (the stream and terminal WebSockets, their limits), and the changelogs of `tungstenite` and
  `tokio-tungstenite` for 0.30.
- **Suggested agent:** Codex, or any coding agent.

## Goal

Dependabot's PR #2 bumps `tungstenite` from 0.29.0 to 0.30.0. Move every user of `tungstenite` and
`tokio-tungstenite` to 0.30 together, with the WebSocket behaviour unchanged.

## What to do

1. **Bump everything in one go:**
   - `crates/api` pins `tungstenite = "=0.29.0"` (dependency and dev-dependency), so pin `=0.30.0`;
   - `crates/runner`'s dev-dependency likewise;
   - the desktop and `fuzz/` use `tokio-tungstenite = "0.29"`: move them to the `tokio-tungstenite`
     release built on `tungstenite` 0.30.

   If that release doesn't exist yet, stop and report: two `tungstenite` versions in one build is
   not acceptable here.
2. **Fix what the compiler flags** (message and frame types, `Bytes` vs `Vec<u8>`, close frames,
   config builders).
3. **Keep every limit and check as it is.** The API sets a maximum message and frame size, refuses
   bad origins and missing tokens, and closes with specific codes. If 0.30 renamed or changed the
   defaults of a config field the code sets, carry the same value over, and list each one in the
   report.
4. **The tests prove it:** `crates/api/tests/{websocket,stream,terminal}.rs`, the runner's
   `terminals.rs`, and the desktop's `no_token.rs`. Don't loosen any of them.
5. **The fuzz target** must still compile.
   - `cargo check --manifest-path fuzz/Cargo.toml` if it builds on stable; otherwise say so, and the
     `fuzz.yml` workflow checks it.
   - Say if a 0.30 change affects what the target feeds the parser.

## Acceptance

- `cargo test -p pitcrew-api -p pitcrew-runner` passes, and the desktop's tests pass if the
  environment can build them.
- fmt, clippy with `-D warnings`, and the guards pass. Every CI job passes on the pull request.

## Out of scope

Other dependency upgrades, and changes to the WebSocket protocol.
