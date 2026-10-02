# Brief 0 · CI green on Linux, Windows and macOS

- **Stream:** 0 · Contracts (integration work). **Branch:** `integrator/ci-green`. **Paths:** any,
  kept to what each fix needs.
- **First read:** [README.md](README.md), the root `CLAUDE.md`, `.github/workflows/ci.yml`, and the
  READMEs of the crates you touch.

## Goal

The first CI run on GitHub (all three systems) failed in places the local checks never covered.
Fix each one, so every CI job passes except the tmux tests (the separate brief `B-tmux-compat`). Also
make the suite behave in a cloud VM, which runs as root.

## What to fix

1. **Windows, `crates/api` `listener::pipe::tests::the_current_user_owns_it_and_alone_has_access`.**
   CI's runner account is Windows' built-in Administrator (RID 500), which SDDL renders as the alias
   `LA`. The test compares the SDDL text with the user's SID string, so it fails: `D:P(A;;FA;;;LA)
   lacks S-1-5-21-…-500`. Compare the owner and the ACE's SID as SIDs (`EqualSid`, or by reading the
   descriptor's parts), never as SDDL text. Check every test that compares SDDL text this way,
   including `crates/remote`'s new pipe tests and `crates/runtime/src/pty/windows.rs`, and fix them the
   same way.
2. **macOS, `crates/daemon` `connect_carries_a_request_to_the_daemon`:** "the bridge ended: Timeout".
   Find why `pitcrewd connect` (the stdio bridge, `pitcrew_remote::bridge`) times out on macOS:
   - socket path length (macOS allows 104 bytes and its temp dirs are long);
   - the peer check (`getpeereid`);
   - or something else.

   Fix the cause, not the timeout. If the bridge itself has a macOS bug, fix it in `crates/remote`.
3. **UI, `apps/ui/src/projects/tests/board.test.tsx`:** "Unable to find … Due 10 Oct". The text depends
   on the machine's date, time zone or locale. Make date-rendering tests independent of all three
   (pin the clock, time zone and locale in the test setup), and check the other date tests for the
   same dependence.
4. **The desktop crate's clippy:** `manual checked division` at
   `apps/desktop/src-tauri/src/remote/mod.rs` ~957 (newer clippy, all three systems). Fix it, and any
   other new lint in that crate.
5. **CI shows every failure:** make the Rust test step run `cargo test --workspace --no-fail-fast`, so
   one failing binary doesn't hide the rest.
6. **Root in the cloud VM:** tests that check a permission is refused (mode bits, unreadable files,
   other users' files) fail when run as root, because root bypasses them. Make each such test skip,
   with a printed reason, when the effective uid is 0. Find them by running
   `cargo test --workspace --no-fail-fast` in the VM. The cloud session that found this saw 22
   failures in 5 targets, 4 of them in `crates/remote`. Don't weaken the tests for other users.
7. **`fuzz/Cargo.lock`** is out of date since the PTY runtime merged. Refresh it so the fuzz
   workspace builds with `--locked`.

## Acceptance

- In the VM, `cargo test --workspace --no-fail-fast` passes except the tmux tests (`B-tmux-compat`),
  and every root skip says why.
- On the pull request, CI passes on Linux, Windows and macOS, apart from the tmux tests.
- fmt, clippy, `npm test`, the UI's checks, and the guards pass.

## Out of scope

tmux 3.3+ compatibility (`B-tmux-compat`), and new features.
