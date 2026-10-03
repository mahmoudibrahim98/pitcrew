# Brief 0 · Docs and small fixes left by today's reviews

- **Stream:** 0 · Contracts (docs across crates, one threat-model row, one comment).
  **Branch:** `integrator/docs-followups`.
  **Paths:**
  - `crates/hub-work/README.md`, `crates/daemon/README.md`, `benches/README.md`;
  - `crates/remote/src/job.rs` (a comment only);
  - `docs/security/threat-model.md`;
  - `crates/cli/src/install/jsontext.rs` and `crates/cli/src/install/claude.rs` (item 4 only).
- **First read:**
  - [README.md](README.md) and the root `AGENTS.md` (or `CLAUDE.md`);
  - the integrator's reviews on PR #30 (`#issuecomment-5968820841`) and the #18 re-review notes in its PR.
- **Suggested agent:** Codex, or any coding agent.

## What to do

1. **The recap cache's placement.** These still describe `recaps.sqlite3` as always living in the state folder:
   - `crates/hub-work/README.md` ~259, ~543;
   - `crates/daemon/README.md` ~66, ~605;
   - `benches/README.md` ~195.

   Since #30, on a network filesystem it goes to a private local folder named after the state folder (or memory). Describe that, matching `crates/hub-work/src/recap_db.rs`.
2. **A stale comment:** `crates/remote/src/job.rs` ~10-11 talks about `unsafe` that is gone.
3. **A threat-model row for the recap cache's fallback folder:** where it goes (temp before `$XDG_RUNTIME_DIR`), its modes, how it's reused only if `lstat` shows our 0700 folder, and what a hard kill leaves behind.
4. **CRLF settings files keep CRLF.**
   - `crates/cli/src/install/jsontext.rs` ~251, ~256 (`append`) and `claude.rs` ~122, ~126 (`braces`/`brackets`) hard-code `\n`. In a CRLF settings file, newly added hook events come out with LF line endings.
   - Use the file's own line ending, as the exec-form code already does.
   - Also fix the double space left when switching exec form back to shell form on a hook object pitcrew created.
   - **Tests:** both cases.

## Acceptance

- fmt, clippy with `-D warnings`, `cargo test -p pitcrew-cli`, and the guards pass. Every CI job passes on the pull request.

## Out of scope

Anything beyond these items.
