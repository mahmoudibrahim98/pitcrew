# Brief H · Terminal WebSocket and activity paging

- **Stream:** H · API and auth. **Branch:** `s/H/terminal-and-activity`. **Paths:**
  `crates/api/**`, `crates/auth/**`.
- **First read:** [README.md](README.md), then `docs/build/contracts/api-v1.md` ("Terminals",
  `GET /v1/events`), `crates/interfaces/src/runtime.rs` (`Runtime`, `OutputChunk`) and its
  `FakeRuntime`, and the merged `crates/api` on `main`.

## Goal

The two remaining read paths the Agent console needs from the API:
- **live terminals** over WebSocket;
- **activity paging** over the event log.

## What to build

0. **First, follow-ups from the delta-stream review** (the branch was merged; these are small):
   - **The hook sink blocks an async worker** (`hooks.rs:74-78`). `deliver` is synchronous and
     runs inside `tokio::spawn`, but the runner's real sink will write to SQLite. Drain on a
     blocking thread, e.g. `spawn_blocking(move || while let Some(e) =
     events.blocking_recv() { sink.deliver(e) })`, and document that `deliver` may block.
     Survive a panicking `deliver`: log it, keep draining, and don't count later events as
     dropped.
   - **Make the ordering test deterministic.** Add a wrapper `EventSource` for tests that:
     - appends inside `latest_rev()` after computing its value;
     - appends inside the first `since()`;
     - broadcasts ranges out of order.

     Assert exact-once, in-order delivery for each case.
   - **The slow-client test can flake.** Give the fast client its own config with a timeout of
     several seconds.
   - **Nits:**
     - wrap the hooks `Path` extractor so invalid UTF-8 gets an `ApiError`, and map body errors
       precisely;
     - clamp `ping_every` above zero;
     - send a Close frame when ending a session;
     - lower `max_message_size` on the stream socket;
     - one `now_ms`;
     - `saturating_add` in `MemorySource`.
   - **Consider moving `StoreSource` behind a crate feature**, so crates that only need
     `pitcrew_api::client` don't pull in SQLite.
1. **A terminal seam.** Define a trait in `crates/api`, e.g. `Terminals`:
   - `attach(session) -> Result<Attachment, …>`, where an attachment yields the output
     (offset-addressed, as `OutputChunk`), accepts input bytes and resizes, and reports exit;
   - an error that maps to `404` (no such session) or `503 unavailable` (machine unreachable).

   The runner (stream D) implements it later on top of `Runtime`. Provide an implementation over
   any `Runtime` (so `FakeRuntime` works in tests) that maps a session to its terminal.
2. **`GET /v1/sessions/{id}/terminal?cols=&rows=`** (device tokens, subprotocol auth), exactly
   per the contract:
   - `truncated` before the replay when the buffer lost the start;
   - replay as binary frames, then live output;
   - client binary frames written as keystrokes;
   - `resize` text frames;
   - unknown control types ignored; malformed JSON closes with 1007;
   - `exit` when the program ends.
   - **Backpressure:** a bounded outbound queue. A slow client is disconnected and resumes by
     offset. Cap frame sizes (e.g. 64 KiB per binary frame).
   - One writer per terminal at a time is fine for now; document what happens with two clients.
3. **`GET /v1/events?before=&limit=`**, from the `EventSource` you built, per the contract:
   - `{events, from_rev, to_rev, at_start}`, oldest first within the page, newest page when
     `before` is absent;
   - `before` is exclusive; default limit 100, max 500.
   - The entity filters (`project`, `workstream`, `task`, `session`) need domain projections
     that don't exist yet. Accept `session` and `task` by matching ids in the event body on a
     bounded scan, and document the limits. Answer `400 invalid` with a clear message for
     `project` and `workstream` until stream E provides an index. Propose the contract note.
4. Update the crate README with the composition-root wiring for both routes.

## Acceptance

- **Terminal**, with `FakeRuntime` through the real router and a WebSocket client:
  - the replay then live output arrive in order, with no byte lost or duplicated across a
    reconnect using the last offset;
  - `truncated` is sent first when appropriate;
  - resize reaches the runtime;
  - input bytes arrive exactly;
  - 1007 on malformed control; unknown types ignored;
  - a slow client is dropped while another keeps receiving;
  - 401 without a token, 403 with an agent token, 404 for an unknown session.
- **Activity:**
  - paging over a 1,200-event source joins up with no gaps or duplicates, and `at_start` is
    right;
  - `session` and `task` filters are correct on the fixture events;
  - `project` gives 400 with the documented message.

## Out of scope

Composing domain crates' routes (the integrator wires `crates/daemon`), TypeScript export.
