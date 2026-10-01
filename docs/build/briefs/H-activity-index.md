# Brief H · Activity filters from the work index, and keepalive under input pressure

- **Stream:** H · API and auth. **Branch:** `s/H/activity-index`. **Paths:** `crates/api/**`,
  `crates/auth/**`.
- **First read:** [README.md](README.md), [H-terminal-hardening.md](H-terminal-hardening.md)
  (merged), `docs/build/contracts/api-v1.md` (activity paging and filters), and `crates/hub-work`
  on `main`: the `EventRefs` trait and `revs_matching(filter, before_rev, limit) ->
  (revs, scanned_to)`, plus its README.

## Goal

`GET /v1/events?project=` and `?workstream=` work on the real hub, `task=` catches every event
about the task, and terminals don't get closed by mistake while input backs up.

## What to build

1. **The `EventRefs` seam:**
   - The activity route accepts an optional `EventRefs` implementation. `RouterParts` or the
     activity state gets a setter; keep the dependency direction, with no `hub-work` dependency in
     `pitcrew-api`. Re-declare the trait here, or take a boxed closure the daemon adapts. Pick the
     simplest that keeps crates decoupled.
   - With an index: `project=` and `workstream=` are answered through it, with the same bounded
     paging semantics as today (`from_rev`, `to_rev`, `at_start`, and an empty page that isn't at
     the start).
   - Without one: they keep answering 400 as now.
   - `task=` and `session=` keep matching by key in the body, which catches ids anywhere.
2. **`task=` coverage:** also match events about the task's **sessions**, such as `file_edited`
   and `tool_ran` for a session linked to that task.
   - Use the index's session-to-task link when an index is present. Otherwise document the gap.
   - Test with the fixture: `task=PAP-1` includes PAP-1's session's `file_edited`.
3. **Keepalive under input pressure** (H review follow-up):
   - In `terminal.rs`'s `session_loop`, `socket.recv()` is gated on `waiting.is_none()`, which
     also stops reading Pongs while input backs up.
   - Read control frames (Ping and Pong) separately from data, or keep reading the socket and
     hold data in a bounded buffer, so a client answering Pings is never closed with 1013 "no
     pong" because input is busy.
   - Test: a stalled write plus continuous input plus Pings over longer than `pong_timeout` must
     not close with 1013.
4. **Contract doc:** update the api-v1 activity section to say project and workstream filters
   work when the hub has its index. Stream 0 owns the doc, so put the change in your report and
   the integrator applies it.

## Acceptance

- Activity tests through the route with a fake `EventRefs`:
  - project and workstream filters;
  - paging across a gap;
  - `at_start`;
  - a 400 when there is no index;
  - `task=` including session events.
- The keepalive test above.
- The existing activity and terminal tests stay green, run three times.

## Out of scope

The daemon wiring (stream 0 adds it after merge), the hook-only token, and new routes.
