# Brief H · Terminal hardening

- **Stream:** H · API and auth. **Branch:** `s/H/terminal-hardening`. **Paths:** `crates/api/**`,
  `crates/auth/**`.
- **First read:** [README.md](README.md), then
  [H-terminal-and-activity.md](H-terminal-and-activity.md) (merged), the current `crates/api` on
  `main`, and `docs/build/contracts/api-v1.md` ("Terminals", `GET /v1/events`; updated with the
  notes you proposed).

## Goal

Close the review findings on the terminal WebSocket **before the runner (stream D) implements the
`Terminals` seam**. Changing the trait after that would be a breaking change.

## What to fix

1. **Bound terminal size:** one shared constant for cols and rows, 1..=1000 (the mock hub's
   limit). Answer 400 on the query and close with 1007 on a `resize` outside it. Test both.
2. **No side effects before the upgrade check.** Today a plain GET with `cols`/`rows` resizes
   the live terminal and then answers 400. Check the upgrade after `attach` (keeping 404 ahead of
   400) and before any resize, or do the first resize inside the session loop. Test: a GET
   without upgrade headers never resizes.
3. **Idle cost, and the seam's shape:**
   - call `exited()` only on rounds where the read returned nothing;
   - back off while idle (20 ms up to about 250 ms), and reset on any input or output;
   - use `more = !chunk.data.is_empty() && offset < chunk.end`, so an empty chunk can't spin;
   - **add an optional push hint to the `Attachment` trait now**, e.g.
     `fn changes(&self) -> Option<tokio::sync::watch::Receiver<u64>>` with a default of `None`.
     When it is present, wait on it instead of polling.
   - Test that an idle attachment makes far fewer calls per second, e.g. at most 5 after the
     back-off.
4. **A terminal that disappears mid-stream is an exit.** `TerminalError::NotFound` from
   `exited()` or `read()` during a stream means the program ended: send `{"type":"exit"}` and
   close with 1000, not 1011.
5. **Document the `Attachment` contract:**
   - `exited()` may return true only once all output is readable;
   - every call returns within bounded time, answering `Unavailable` rather than hanging.

   Enforce a timeout around each blocking call (e.g. 5 s, giving 503 or 1011) so a stalled
   runtime can't pile up blocking-pool threads. Test it with a fake that hangs.
6. **Keepalive:** send a WebSocket Ping about every 20 s on terminal sockets, and close when no
   Pong arrives within a timeout. Test it with a client that stops answering.
7. **Activity:**
   - match `session` and `task` filters by walking the parsed JSON by key (as the test's
     reference does), not by substring;
   - hoist the needle out of the per-event closure;
   - the contract now says that empty non-final pages can happen and only `at_start` ends
     paging. Keep that behaviour, and put the page type into `pitcrew-protocol` (see below).
8. **Nits:**
   - `max_frame_size` on the terminal socket (as the stream has);
   - flush the Close reply before dropping the socket (both stream and terminal), so browsers
     see a clean close;
   - log the hook sink's panic payload, and say in the docs that the sink keeps receiving after
     a panic;
   - README: `terminals.clone()` in the wiring example; the stream's close codes;
   - `#[doc(hidden)]` on the public `pump` helpers;
   - 1001 on hub shutdown;
   - a WebSocket-level test for 1013 slow-client closing and for two clients on one terminal.
9. **Use the protocol's `EventsPage`** (added to `crates/protocol/src/api.rs` on `main`) instead
   of your own `activity::EventsPage`.

## Acceptance

Every item above has a test. All checks pass, including
`cargo clippy -p pitcrew-api --no-default-features --all-targets -- -D warnings` and the Windows
target for `pitcrew-api` without the `store` feature.

## Out of scope

Implementing `Terminals` for real runtimes (stream D).
