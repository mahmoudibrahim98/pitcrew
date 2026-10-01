# Brief A · Ingest tests that don't flake under load

- **Stream:** A · Ingest. **Branch:** `s/A/test-robustness`. **Paths:** `crates/ingest/**`.
- **First read:** [README.md](README.md), the `crates/ingest` README and `tests/`
  (`codex.rs`, `opencode.rs`, `claude.rs`, and their `*.proptest-regressions` files).

## Goal

Two property tests failed once each while about six builds ran at once:
- `codex.rs` `fixture_in_random_chunks_matches_one_read`, at `read_all`'s `.expect("read")`;
- `opencode.rs` `random_batches_match_one_read*`, at `history()`'s `.expect("history")`.

Both passed on every rerun. The cause is the environment: under heavy load, WSL's reads of files on
the Windows drive (`/mnt/c`) occasionally fail, and these tests read the same fixture file again
in every proptest case, hundreds of times per run.

## What to build

1. **Read each fixture once per test binary** (`OnceLock` or `LazyLock`) and hand out the bytes,
   instead of reading the file inside every case. The same goes for any other file a property
   test reads repeatedly.
2. Where a test must write and re-read a file (the chunked-append tests), write it under the
   system temp directory (WSL's own filesystem in CI and here). Check the tempdir crate does that
   already, and say so.
3. **A transient read error must never be the test's failure.** If a read of a file the test
   just wrote fails with an I/O error, retry once and fail only on a second error, with the error
   in the message. Never silence a parse or logic failure.
4. **Keep the proptest regression files as they are,** and check their saved cases still pass.

## Acceptance

- Each of the two tests, run 20 times in a row, passes every time. Report the counts.
- No test asserts less than before: show a diff summary of the assertions.
- fmt, clippy (Linux and Windows target), `cargo test --workspace`, and the guards all pass.

## Out of scope

Changes to the adapters themselves.
