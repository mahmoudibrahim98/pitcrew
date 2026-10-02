# Brief 0 · One trust check for every binary we launch, one copy of the Windows pipe security

- **Stream:** 0 · Contracts (integration work across crates). **Branch:** `integrator/trust-sweep`.
  **Paths:**
  - a new shared crate (e.g. `crates/platform-trust`, named `pitcrew-trust`), registered in
    `docs/build/ownership.json`;
  - `apps/desktop/src-tauri/src/daemon/locate.rs`, `crates/daemon/src/runtime.rs`,
    `crates/runtime/src/pty/**`, `crates/api/src/listener/**`, `crates/remote/src/pipe_security.rs`;
  - the four lock guards (`crates/runner/src/store.rs`, `crates/auth/src/private.rs`,
    `crates/daemon/src/runtime.rs`, `crates/ptyd/src/server.rs`), comments only;
  - `docs/security/threat-model.md` and `Cargo.lock`.
- **Start after PR #9 (0-daemon-pty) merges.** This brief changes the `Plan::choose` and ptyd launch
  that #9 adds. Branch from `origin/main` once #9 is in.
- **First read:**
  - [README.md](README.md) and the root `CLAUDE.md`;
  - the reviews on PR #9 (`#issuecomment-5962544691`) and PR #11 (`#issuecomment-5962544997`);
  - the READMEs of `crates/runtime`, `crates/ptyd` and `apps/desktop/src-tauri` ("Locating the
    daemon").

## Goal

The desktop checks `pitcrewd` before running it (`check_trusted`, `locate.rs` ~115-179):
- **on Unix:** owner and mode of the file, the file it resolves to and both folders;
- **on Windows:** the downloaded-from-the-internet mark (Zone.Identifier).

The daemon then starts `pitcrew-ptyd`, which starts every agent, after checking only that it is an
absolute path to a file with the exec bit. ptyd is also launched at the first terminal, possibly hours
after the daemon checked it. Give every binary we launch the same check, done just before launch.
Along the way, merge the three copies of the Windows pipe-security code into one.

## What to build

1. **A shared trust crate:**
   - move `check_trusted` out of the desktop into it, with its tests;
   - the desktop's `locate.rs` calls it, with no change in behaviour (its tests still pass);
   - keep it small, with no async and no dependencies beyond what `check_trusted` already uses.
2. **Check ptyd before every launch:**
   - **In `Plan::choose`:** if ptyd fails the check, warn with the reason and run without the PTY
     runtime, as when ptyd is missing.
   - **Again in `pty::launch::launch`, just before spawning:** refuse with the reason, and the
     terminal request fails cleanly.
   - **Tests:**
     - group- or world-writable ptyd;
     - a symlink into a writable folder;
     - on Windows, a ptyd carrying a Zone.Identifier stream;
     - a ptyd made writable after `choose`, refused at launch.
3. **One copy of the Windows pipe security:**
   - `crates/api/src/listener/pipe_security.rs`, `crates/remote/src/pipe_security.rs` and
     `crates/runtime/src/pty/windows.rs` each carry their own SID lookups, owner/DACL reads and SDDL
     parsing.
   - Move the shared parts into the trust crate, behind `#[cfg(windows)]`, with one test suite:
     - current user SID;
     - default owner;
     - owner and DACL of a handle;
     - SID from text;
     - integrity label;
     - the DACL type that records the access mask (#11 made the tests assert `GA` read back as `FA`).
   - Keep each caller's own policy where it is.
   - The DACL comparisons stay SID-based, as #7 made them.
4. **A per-endpoint lock on Windows** (`runtime.rs` `lock_endpoint`, a no-op there today):
   - take a `LockFileEx` lock on a per-endpoint file under `%LOCALAPPDATA%\pitcrew\` and hold it in
     `Locked`, so two daemons can't share one ptyd through `--ptyd-endpoint`;
   - `File::try_lock` needs Rust 1.89 and the minimum is 1.88, so go through `windows-sys`, or raise
     the minimum Rust version and say so;
   - then let the "third daemon" test in `crates/daemon/tests/pty.rs` run on Windows too.
5. **Comments on the four lock guards:** `LOCK_UN` releases the lock on the shared open file, so a
   child forked to keep a lock would lose it when the parent drops the guard.
6. **The threat model:**
   - the ptyd trust check (and whether it closes T-items about planted binaries);
   - the endpoint lock;
   - one row saying the pipe security now has one implementation.

## Acceptance

- fmt and clippy (`-D warnings`) pass on Linux in the VM, and CI's Windows and macOS jobs pass. Every
  CI job passes on the pull request.
- `cargo test --workspace --no-fail-fast` passes in the background in the VM, as do the desktop's
  tests and the guards.
- The integrator re-runs the Windows tests natively with `PITCREW_REQUIRE_PTYD=1` before merging.
  Write the report so it says which tests need a real Windows machine.

## Out of scope

Signing, and new checks beyond what `check_trusted` already does.
