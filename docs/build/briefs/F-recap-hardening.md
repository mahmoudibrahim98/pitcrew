# Brief F · Recap engine: hidden text, firm links, bounded directory

- **Stream:** F · Recap and back office. **Branch:** `s/F/recap-hardening`. **Paths:**
  `crates/recap/**` (+ `Cargo.lock`).
- **First read:** [README.md](README.md), the `crates/recap` README, the `crates/hub-work` README
  ("Recaps", and the session-link caveat), `crates/hub-work/src/projection/sessions.rs` (the
  firm-link and agent-retention rules), `crates/sync-github/src/bounds.rs` (`is_hidden`, now
  extended), and `docs/security/threat-model.md` (untrusted text into agents).

## Goal

Four fixes that the reviews of the hub's recap index and of the GitHub and Jira sync found in the
engine. Recaps stay deterministic and every clause keeps its receipts.

## What to build

1. **Hidden text.** `clean` (`text.rs`) also strips:
   - Unicode tag characters U+E0000–E007F, used to smuggle hidden text into model prompts (recap
     text reaches agents);
   - U+00AD (soft hyphen), U+180E (Mongolian vowel separator), U+2028 and U+2029.

   Keep the set identical to `pitcrew_sync_github::bounds::is_hidden`, and say so in both places'
   docs. Snapshots must not change unless a fixture contains these characters. If one does,
   explain the change.
2. **Firm links win.** `Directory::add_session` replaces a session's links today, so a re-stated
   `session_discovered` (the runner re-states sessions with no links) unlinks the session in
   recaps, while the hub keeps the firm link.
   - Follow `crates/hub-work`'s rules: a firm link (dispatch, manual) is not replaced by an
     inferred or empty one, and a known agent is kept when a re-statement names none.
   - Same for `session_linked` with an inferred basis after a manual one.
   - Test each rule, and that the hub's recap index (with your change) still equals a rebuild.
     Run `cargo test -p pitcrew-hub-work`.
3. **A bounded directory without a first-come cap.** `MAX_ENTRIES` is first-come today: an index
   replaying from revision 1 stops learning sessions after the 100,000th, for good.
   - Prune ended sessions (and their dispatches and asks) once nothing open refers to them, or
     use an LRU. The result must be deterministic: the same events give the same directory
     whatever the batching (keep the property test).
   - Document the bound.
4. **Stale moves.** A `task_moved` whose `from` doesn't match the task's current status (only a
   second writer can produce one) still counts as a move today. Follow the work model: count it
   as the hub does (check `crates/hub-work`), and test it.
5. **A public `Directory::ask`** (or a "names changed" signal), so the hub's index can stop
   tracking ask descriptions itself. Coordinate through your report; don't edit hub-work.

## Acceptance

- The tests above pass. `cargo test -p pitcrew-recap` snapshots are unchanged, or each change is
  explained.
- The property tests still pass: incremental equals pure, and hostile input never panics.
- `cargo test -p pitcrew-hub-work` passes, including its recap property tests.
- fmt, clippy (Linux and Windows target), `cargo test --workspace`, `cargo deny check`, and the
  guards all pass.

## Out of scope

Persisting blocks (stream E), read cursors, and model-written summaries.
