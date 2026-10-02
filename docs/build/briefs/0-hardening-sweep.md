# Brief 0 · Hardening sweep: transcript symlinks, one hidden set, fuzz housekeeping

- **Stream:** 0 · Contracts (integration work across A, F, G, I and Q). **Branch:**
  `integrator/hardening-sweep`. **Paths:** `crates/ingest/**`, `crates/recap/**`,
  `crates/sync-github/**`, `crates/cli/**`, `crates/protocol/**` (only if the shared set goes
  there), `fuzz/**`, `docs/security/threat-model.md`, `Cargo.lock`.
- **First read:** [README.md](README.md), `docs/security/threat-model.md` (§6's findings R10 and
  R24 to R33, the open items O29 to O40, and the fuzzing section), the `crates/ingest` README
  (discovery and reading), `crates/recap/src/text.rs`, `crates/sync-github/src/bounds.rs`,
  `crates/cli/src/display.rs`, and `fuzz/README.md`.

## Goal

Three loose ends from reviews, closed together:
- a transcript can't be swapped for a link to another file after discovery;
- every place that drops hidden characters drops the same ones;
- the fuzz regressions and the threat model say what is fixed.

## What to build

1. **Transcript reads don't follow a final symlink.** Discovery finds a transcript, and the read
   happens later. A transcript file replaced in between by a symlink (to `~/.ssh/id_ed25519`,
   say) or by anything that isn't a regular file must not be read.
   - **Unix:** open with `O_NOFOLLOW`, then check the opened file is regular (`fstat`), so there
     is no window between a check and the open.
   - **Windows:** open with `FILE_FLAG_OPEN_REPARSE_POINT`, then refuse a reparse point or a
     non-regular file, by the opened handle's attributes.
   - **Only the transcript file itself** is checked this way. Directories above it may be links
     (a home on another drive), as discovery already allows.
   - A refused file is a read error of its own kind, logged once per file without its contents,
     and the session stops updating rather than failing the runner.
   - Apply it to every adapter's read path (Claude Code, Codex, OpenCode), and to the daemon's
     transcript stand-in if it opens files itself. Report if it does; the daemon's paths are not
     yours, so describe the change for stream 0 rather than making it.
2. **One set of hidden characters.** The CLI's set is the union. The recap's `is_hidden`
   (`clean`, `clean_tail`) and sync-github's `is_hidden` lack U+034F, U+115F, U+1160, U+3164,
   U+FFA0, U+FE00–FE0F, U+E0100–E01EF and U+FFF9–FFFB.
   - Give the three one definition, in the lowest crate they all already depend on (probably
     `pitcrew-protocol`, e.g. `text::is_hidden`), if that adds no dependency. Otherwise keep
     the copies, with a test that pins them equal.
   - The pinned-table tests in each crate still pass, updated together.
   - Keep each crate's own extra rules (e.g. `clean_tail` and U+2028/U+2029).
3. **Fuzz housekeeping:**
   - Every `fuzz/regressions/**/open-*` input is now fixed: R10's residuals, R25–R27 (J), R28–R32
     (G) and R33 (F). Run each through its target to confirm, then rename it without `open-`, so
     CI fails if it regresses. An input that still fails stays `open-`, and is reported.
   - `cli_hooks`: R24 is fixed (Codex's installer keeps a BOM), so remove the target's Codex BOM
     carve-out and check the round trip exactly.
   - `github_links`: exercise `origin::trusted_next_url`, the seam the client uses now, rather
     than an older helper, if it doesn't already.
   - Run each changed target for 60 s, and report the executions.
4. **The threat model:** mark R10's residuals, R24 and R25–R33 fixed, each with where. Close O29
   (if the `%2F` residual is covered; else say what remains), O30, O34, O38, O39 and O40. Add a
   row for the transcript symlink rule.

## Acceptance

- Tests:
  - **Unix:** a transcript swapped for a symlink after discovery isn't read (also a symlink to a
    directory, and a FIFO, which must not hang).
  - **Windows** (behind `cfg(windows)`, compiled by the Windows clippy): the same for a symlink
    where creating one is allowed; skip with a message where it isn't.
  - A normal transcript under a symlinked home is still read.
  - Each hidden-set addition is dropped by all three crates.
- The renamed regression inputs pass through their targets, and the fuzz workflow's "fixed
  findings stay fixed" step would run them.
- fmt, clippy (Linux and Windows target), `cargo test --workspace`, `cargo deny check`,
  `npm test`, and the guards all pass. The fuzz workspace builds.

## Out of scope

The daemon's own files (stream 0's daemon-setup brief is changing them), new fuzz targets, and
the rest of the threat model.
