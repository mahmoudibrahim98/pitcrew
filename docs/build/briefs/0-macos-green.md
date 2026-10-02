# Brief 0 · CI green on macOS

- **Stream:** 0 · Contracts (integration work). **Branch:** `integrator/macos-green`. **Paths:** any,
  kept to what each fix needs (mostly tests).
- **First read:** [README.md](README.md), the root `CLAUDE.md`, `.github/workflows/ci.yml`, and the
  READMEs of the crates you touch.

## Goal

Now that CI runs every test binary (`--no-fail-fast`), about 70 macOS test failures show up that were
always there, hidden behind the first failing binary. Almost all are tests that assume Linux. Make the
macOS jobs (`Rust (macos-latest)` and `Desktop shell (macos-latest)`) pass, without weakening what the
tests check on Linux and Windows.

## What to fix

The full list of macOS failures is in the comment on PR #8 (`#issuecomment-5958732266`), and the
review of #8 read the macOS CI log (run 37045453366).

0. **First, a possible security gap:** `crates/remote/tests/deploy.rs`
   `the_way_to_the_root_is_checked` deploys on macOS where it should refuse. The helper's check of the
   way to its root (the folders above `~/.pitcrew` must not be writable by others) may silently pass on
   macOS, for example because BSD `stat`, `ls` or `find` flags differ from GNU's, so a check reads
   nothing and succeeds.
   - Find out whether the production check (the remote helper script or the deploy code), not just the
     test, accepts a path it should refuse on macOS or BSD.
   - If it does, fix it so it fails closed on any platform where it can't tell, and add a test.
   - Say clearly in your report which it was.

The causes found so far:

1. **Long macOS temp folders.** macOS's `$TMPDIR` (`/var/folders/…/T/`) makes socket paths too long:
   - "too long for a control socket path": `crates/remote/src/private.rs` ~227 and ~270,
     `tunnel/supervisor.rs` ~1331, `tests/deploy/tunnel.rs` ~57 (about 25 tests), `tests/fake_ssh.rs`
     (about 15), and the desktop's `src/remote/tests.rs` (~275, ~437, ~454, ~564);
   - "socket path … longer than 100 bytes": `tests/deploy/slurm.rs` ~1644 and ~2060,
     `tests/deploy/tunnel.rs` ~494, ~1264, ~1811 and ~1885.

   Fix: a shared test helper that makes test temp roots under `/tmp` on macOS (e.g.
   `tempfile::Builder::new().tempdir_in("/tmp")`), and use it wherever a test makes a socket. If any
   *production* default path can exceed the limit on macOS (not just tests), fix that too and say so.
2. **`crates/ptyd/src/spawn.rs` ~317:** macOS ships `/usr/bin/cd`, so the test's assumption about
   shell builtins is wrong there. Make the test platform-correct.
3. **`crates/remote/tests/fake_ssh.rs`:**
   - ~709 uses `/bin/false`, which on macOS is `/usr/bin/false`;
   - ~337: the probe tests fail with "ssh was not run: NotFound".

   Find out why, and fix it.
4. **`crates/remote/tests/deploy.rs`** assumes GNU/Linux tools. The failing places are ~1430 (the host
   is `MacOs`, not an unknown platform), ~1595 (BSD `mv`), ~1652, ~1914 (ACLs), ~835 (umask), ~1144
   and ~1171 (locks), ~2015 (the direct launcher), plus `slurm.rs` ~1027. Make the sandbox
   platform-aware. If a test only makes sense against a Linux remote, say why when you mark it
   Linux-only.
5. **`crates/runner/tests/common/mod.rs` ~57 and ~67:** `CollectSink` waits on one `Condvar` with two
   different mutexes (`events` and `closed`), and macOS std panics ("attempted to use a condition
   variable with two mutexes"). That is undefined use everywhere, not just on macOS. Use one mutex
   over a single state struct, or two condvars.
6. **Lock flakiness on macOS:** `crates/runner/tests/terminals.rs` ~145 (`Store(Locked)`), and
   `crates/daemon/src/runtime.rs` ~674 ("another pitcrewd uses this tmux socket", seen once). Find the
   cause (a lock not released between tests, `flock` semantics, a shared path), and fix it.

Leftovers from the review of #8:

7. **`crates/remote/src/bridge.rs` ~543 (`half_closes_pass_through`):** the test can't tell if a
   half-close were wrongly treated as a hang-up. After reading "bye", wait about 500 ms and assert the
   pump is still running before the client writes.
8. **`bridge.rs` ~442 and ~462-465:** build the `Timespec` from `HANG_UP_WAIT`, so the two values
   can't drift apart.
9. **Pipe tests** (`crates/api/src/listener/pipe.rs`, `crates/runtime/src/pty/windows.rs`,
   `crates/ptyd/src/server.rs`): `Ace::Allow` ignores the access mask. Record it and assert the grant
   the code creates (`GA`, which reads back as `FA`).
10. **`apps/ui/tests/clock.ts`:** a one-line comment saying to call `vi.useFakeTimers()` before
    `vi.setSystemTime()`. Otherwise `useRealTimers()` restores the unpinned `Date`.

## Acceptance

- On the pull request, `Rust (macos-latest)` and `Desktop shell (macos-latest)` pass. Linux and
  Windows jobs stay green, apart from the tmux tests if #7 hasn't merged yet.
- Every test you mark Linux-only or Unix-only says why, in a comment.
- In the VM: fmt, clippy, `cargo test --workspace --no-fail-fast` (in the background; mind the disk),
  `npm test`, and the guards. macOS is checked only by CI, so push early and iterate on CI's results.

## Out of scope

New features; tmux (PR #7).
