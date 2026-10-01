# Brief I · Hooks keep a BOM; one set of hidden characters

- **Stream:** I · CLI and hooks. **Branch:** `s/I/hygiene`. **Paths:** `crates/cli/**`
  (+ `Cargo.lock`).
- **First read:** [README.md](README.md), the `crates/cli` README, `docs/security/threat-model.md`
  (finding R24), `fuzz/regressions/` (if R24 has an input there), `crates/recap/src/text.rs`
  (`is_hidden`) and `crates/sync-github/src/bounds.rs` (`is_hidden`).

## Goal

Two small fixes from the fuzzers and the reviews.

## What to build

1. **R24:** installing the Codex hook drops a leading UTF-8 BOM from `config.toml`, and
   uninstalling doesn't put it back. Keep the file's BOM (and its line endings) exactly as found,
   on install and on uninstall, for every agent's config file that has one.
   - Test it: a BOM-prefixed file with CRLF line endings, install then uninstall, gives a
     byte-identical file.
2. **One set of hidden characters.** The CLI's `display.rs` strips a different set than the recap
   engine's `clean` and sync-github's `is_hidden` (it also drops U+FFF9–FFFB, and lacks U+180E and
   U+2028/2029).
   - Strip the union: tag characters U+E0000–E007F, U+00AD, U+180E, U+034F, U+FFF9–FFFB,
     variation selectors, the Hangul fillers, and the bidi and zero-width set.
   - Show U+2028/2029 as a space.
   - Write the set out in one place in the CLI, and pin it with a test over every code point, as
     recap does.
   - Say in your report which characters the other crates should add, so they converge. Don't
     edit them.

## Acceptance

- The tests above pass. The CLI's existing hook tests pass unchanged.
- fmt, clippy (Linux and Windows target), `cargo test --workspace`, `cargo deny check`, and the
  guards all pass.

## Out of scope

A hook-only token (still waiting on a decision), and the other crates' sets.
