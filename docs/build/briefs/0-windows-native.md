# Brief 0 · Green on native Windows

- **Stream:** 0 · Contracts (integration work across streams). **Branch:**
  `integrator/windows-native`. **Paths:** any, kept to what each fix needs.
- **First read:** [README.md](README.md), and the failing tests' crates' READMEs as you reach them.

## Goal

The whole workspace builds, passes clippy and passes its tests **natively on Windows**, as it does
on Linux. The first native run of `main` (in the integrator's notes) gave 1369 passed, 9 failed,
4 doctest suites failed, and one clippy error. Several failures are real Windows bugs, one of them
a privacy bug.

## What to fix

1. **Privacy: `--demo` watches a home on Windows.** `crates/daemon/tests/runner.rs`
   `a_demo_watches_no_home_of_its_own` fails on Windows: a `--demo` start without `--homes` watched
   the default homes (the test's decoy home), which on a person's machine would be their real
   sessions.
   - Find why: the default-home logic on Windows uses `USERPROFILE`, `APPDATA` or `LOCALAPPDATA`, not
     just `HOME`.
   - Fix it, so that `--demo` watches nothing on every platform.
   - **Audit every test harness** that clears `HOME`, `CLAUDE_CONFIG_DIR`, `CODEX_HOME` or
     `XDG_DATA_HOME`. On Windows it must also point `USERPROFILE`, `APPDATA` and `LOCALAPPDATA` (and
     anything else a home lookup reads) at temp dirs, so **no test can reach a real home on
     Windows**.
   - Report what you found.
2. **Named pipes owned by Administrators.** On this machine a process's default owner is
   `BUILTIN\Administrators` (S-1-5-32-544), so a pipe a server creates is owned by that group, and
   the CLI refuses it: "the pipe is owned by another user" (`crates/cli/tests/named_pipe.rs`, two
   tests).
   - Make every pipe server set an explicit owner, the current user's SID, in its security
     descriptor: the daemon's listener (`crates/api/src/listener/pipe_security.rs`) and any test
     server.
   - Clients keep requiring the user's SID.
   - `pitcrew-ptyd` is being changed on another branch (`s/B/pty-runtime`); leave `crates/ptyd` and
     `crates/runtime/src/pty` alone, and note there whether it needs the same fix.
3. **Check the body before the runtime.** On Windows (no tmux), `POST /v1/sessions` answers 503
   before checking the body, so a missing `cwd` is 503, not 400
   (`crates/daemon/tests/terminals.rs`). Validate the request first on every platform.
4. **Paths:**
   - The scan's `by_folder` paths mix separators on Windows (`<ROOT>\work/proj-a`; the insta
     snapshot `crates/ingest/tests/snapshots/scan__scan_all_engines.snap`). Build them with real path
     joins, and make the snapshot's redaction normalise separators, so one snapshot serves both
     platforms.
   - `crates/remote` tunnel tests build Unix control-socket paths (`/run/user/1000/…`,
     `/tmp/pc/login`). Make them Unix-only, or platform-correct, and check that the Windows code path
     is right as it is (Windows' OpenSSH has no ControlMaster; Windows uses the stdio transport).
5. **The hook installer's quoting** (`crates/cli/src/install/claude.rs` ~719): on Windows the
   command quotes the program's path. Check which shell Claude Code runs hooks in on Windows (the
   CLI README or install docs say, or the installer's own comments), make sure the quoting is right
   for it, and make the test platform-aware.
6. **`crates/ingest/tests/links.rs`** `a_transcript_swapped_for_a_link_to_a_folder_is_not_read`:
   `mklink /J` failed. Create the junction another way (std or a small helper), or skip with a
   message when it can't be made, and say why it failed.
7. **The `pitcrew-api` unit test** that fails on Windows: find and fix it.
8. **Clippy on current stable (1.99):** `question_mark` in `crates/auth/src/token.rs` ~37, and any
   other new lint. Update WSL's toolchain with `rustup update stable`, so Linux and Windows clippy
   agree.
9. **Doctests:** on this machine the merged doctest binaries are written to `%TEMP%`, which the IT
   policy refuses to run. Set `TEMP` and `TMP` to a folder under `C:\DEV` for native runs. That is the
   machine's setup, not a code change. If any doctest still fails, fix it.

## Acceptance

- **Native Windows:** `cargo clippy --workspace --all-targets --locked -- -D warnings` and
  `cargo test --workspace --locked --no-fail-fast` both pass, with zero failures.
- **Linux (WSL):** fmt, clippy and `cargo test --workspace --locked --no-fail-fast` still pass.
- `cargo deny check`, `npm test`, and the guards pass.
- For item 1, a test that fails on Windows if `--demo` watches any home, and a check that no test
  harness leaves a Windows home variable pointing at the real profile.

## Out of scope

`crates/ptyd` and `crates/runtime/src/pty` (stream B's open branch), new features, and the desktop
crate (its own workspace; a later round).
