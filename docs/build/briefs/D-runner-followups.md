# Brief D · Runner follow-ups: sub-agents' hooks, transcript pages, steady tests

- **Stream:** D · Runner. **Branch:** `s/D/runner-followups`. **Paths:** `crates/runner/**`
  (+ `Cargo.lock`).
- **First read:** [README.md](README.md), the `crates/runner` README (all of it: discovery, the
  index, held hooks, the hook ownership rule, `SessionAgents`, `RunnerTerminals`, the commands),
  `docs/build/contracts/api-v1.md` ("Transcript paging"), and the daemon's stand-in,
  `crates/daemon/src/transcripts.rs` (read it; don't edit it).

## Goal

The runner judges a sub-agent's early hooks by its parent's agent, serves transcript pages
itself so the daemon can drop its stand-in, and its tests stop failing under load.

## What to build

1. **Sub-agents' held hooks.** At discovery, a sub-agent's held hooks are judged before the hub
   has stored the sub-agent's session. So `agent_of` doesn't know it, and its parent's agent is
   never seen.
   - Judge them with the parent's agent (`agent_of(parent)`) when the transcript names a parent,
     or pass the parent in.
   - The ownership rule is otherwise unchanged: an agent token only for its own sessions, and a
     device token for sessions with no agent or one its person owns. `Unknown` still denies.
   - Tests:
     - at discovery, a sub-agent of a session running as agent X takes X's held hook;
     - another agent's held hook is refused;
     - a person's hook follows the device rule.
2. **`transcript_page` on the runner's handle,** per api-v1's "Transcript paging":
   - Tail-first; `before`, `limit` (default 200, 1000 at most), `from`, and the
     "nothing older" flag, exactly as the contract says.
   - It reads only transcripts the runner watches, found by the session id in its index, through
     the adapter's own read (so the hardening sweep's no-follow open applies when it lands).
   - An unknown session is a distinct error. A transcript that is gone or unreadable is another.
     The daemon maps them to 404 and 503.
   - Tests: paging back to the start, a partial last line, a transcript growing between two
     pages, and the limits.
   - In the README, describe for stream 0 how the daemon's stand-in is replaced. Don't edit the
     daemon.
3. **Tests that don't fail under load:**
   - `tests/commands.rs`, `a_started_session_is_linked_to_its_terminal_and_driven_through_it`: it
     timed out on "discovery" under load twice. Make discovery deterministic (call `rescan()` and
     wait for the session, not a fixed sleep), with a generous ceiling.
   - `tests/terminals.rs`, `a_hung_call_times_out_and_a_full_pool_answers_at_once`: make it
     independent of machine load (gate on events or channels rather than wall-clock margins).
   - Run each 20 times while the rest of the workspace's tests run in parallel, and report the
     result.
4. **Attribution of hook-caused state** (a proposal, no code): today, state a hook causes is
   authored as the session's person. Write a short section in the runner README with the options:
   a `StoreSink` rule, or a protocol field saying which hook caused an event. Give the trade-offs
   and your recommendation.

## Acceptance

- The tests above pass, and the existing runner tests are unchanged in what they check.
- fmt, clippy (Linux and Windows target), `cargo test --workspace`, `cargo deny check`, and the
  guards all pass.

## Out of scope

The daemon (stream 0 replaces its stand-in later), the dispatcher and session-id adoption (after
the tmux runtime), and `Terminals::attach`'s caller (stream H, before multi-person).
