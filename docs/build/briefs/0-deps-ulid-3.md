# Brief 0 · Upgrade `ulid` to 3

- **Stream:** 0 · Contracts (a workspace dependency used by several streams).
  **Branch:** `integrator/deps-ulid-3`.
  **Paths:**
  - the root `Cargo.toml` and `Cargo.lock`;
  - `apps/desktop/src-tauri/Cargo.lock`, if it changes;
  - the code that uses `ulid` in `crates/{auth,hub-work,office,protocol,recap,runner,store}`.
- **First read:** [README.md](README.md), the root `AGENTS.md` (or `CLAUDE.md`), `crates/protocol`'s
  README (ids), and `ulid`'s changelog for 2.0 and 3.0.
- **Suggested agent:** Codex, or any coding agent. The compiler guides most of it.
- **At the same time,** `0-memory-budget` is reworking `crates/hub-work/src/recap.rs`. Keep your
  edits there to what the upgrade needs, so whichever merges second has a small conflict.

## Goal

Dependabot's PR #3 bumps `ulid` from 1.2.1 to 3.0.0, but it only changes the manifests, and the code
doesn't build against 3. Move the workspace to `ulid` 3 **without changing any id that is already
stored or sent**.

## What to do

1. Set `ulid = { version = "3", features = ["serde"] }` in the root `Cargo.toml`, or whatever 3's
   feature names are now. Then fix every use the compiler flags: `crates/protocol/src/ids.rs`,
   `crates/auth/src/token.rs`, `crates/recap/src/{brief,directory,summary,text}.rs`,
   `crates/hub-work/src/{office,recap}.rs`, and the tests that build ids.
2. **Ids must read and write exactly as before.** Ids live in `hub.db`, in the event log, in tokens
   and in the API.
   - Add a test in `crates/protocol` that pins it: a fixed ULID string parses, and prints back the
     same 26 characters.
   - Its serde JSON is the same string, byte for byte.
   - An id made from a fixed timestamp and random value has the same text as under 1.2.
   - If 3 changed the text form, the case, or how serde writes it, stop and report rather than
     migrating stored data.
3. **Keep how ids are made the same:**
   - If the code relies on monotonic generation (`Generator`) or on ordering by time, keep that
     behaviour, and say how 3 provides it.
   - If 3 changed where its randomness comes from, say what it uses now. Tokens in `crates/auth`
     must still come from the OS's secure random source (`getrandom`), not from `ulid`.
4. Check `cargo tree -d` and say if the upgrade leaves two `ulid` versions in the build.

## Acceptance

- `cargo test -p` passes for each of the seven crates, plus fmt, clippy with `-D warnings`, and the
  guards.
- Every CI job passes on the pull request.
- The report lists each API change you followed, and the new id test.

## Out of scope

Other dependency upgrades, and changing what ids look like.
