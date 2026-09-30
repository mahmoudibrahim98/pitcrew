# Brief H · Delta stream and hook intake

- **Stream:** H · API and auth. **Branch:** `s/H/delta-stream`. **Paths:** `crates/api/**`,
  `crates/auth/**`.
- **First read:** [README.md](README.md), then `docs/build/contracts/api-v1.md` ("Live updates"
  and "Hooks"), `StreamFrame` in `crates/protocol/src/api.rs` (note `Hello.log`), the current
  `crates/api` and `crates/auth` on `main`, and the `Store` API in `crates/store` (`since`,
  `latest_rev`, `subscribe`).

## Goal

The live update channel every UI view depends on, **`GET /v1/stream`**, served from the event
log with exact resume. Also the intake route for agent hooks.

## What to build

1. **An event source seam.** Define a small trait in `crates/api`, e.g. `EventSource`:
   - `log_id()`, `latest_rev()`, `since(rev, limit)`;
   - `subscribe()` returning new revision ranges.

   Implement it for `pitcrew_store::Store`. `Store::log_id()` is being added by brief
   C-projections; until it lands, take the log id from the caller, and say so in your report.
   Tests use an in-memory fake.
2. **`GET /v1/stream?since=`** (device tokens; subprotocol auth already exists):
   - the first frame is `hello {rev, log}`;
   - if `since < rev`, replay the missed events in `events` frames of at most 500;
   - then live batches, coalescing new revisions over 50–100 ms;
   - `ping` every 20 s.
   - **Subscribe before reading `latest_rev`**, and skip ranges already sent, so nothing is lost
     or duplicated between the replay and live phases (see the store's documented pattern).
   - **Backpressure:** a bounded per-client queue. A slow client is disconnected, not buffered
     without limit; it will resume with `since`.
   - If the store's broadcast reports `Lagged`, re-read from the last sent revision.
3. **`POST /v1/hooks/{engine}/{event}`** (agent scope):
   - validate `engine` and the event name (`[A-Za-z][A-Za-z0-9_-]{0,63}`), with a 1 MiB body
     cap and a JSON object body;
   - answer **202 immediately** and pass the payload, with the `Caller`, to a `HookSink` trait
     through a bounded channel;
   - when the channel is full, drop the event and count it, rather than blocking the agent.

   The runner (stream D) implements the sink later; provide a no-op or logging sink for now.

## Acceptance

- **Property test:** for random interleavings of appends and reconnects with `since`, the client
  sees every revision exactly once, in order.
- A slow client (never reads) is disconnected once its queue is full, while other clients keep
  receiving. Memory stays bounded; assert the queue cap.
- `hello.log` matches the source; a reset source (new log id) is visible to the client.
- Hooks: 202 for a valid payload; 400 for a bad engine or event name or a non-object body; 413 or
  400 over 1 MiB, as the contract says (400 `invalid`); 401 without a token; a full sink channel
  still answers 202 and increments the drop counter.
- End to end over the real unix socket (Unix), with the subprotocol auth.

## Out of scope

Terminal WebSockets, `GET /v1/events` paging, and composing domain crates (later briefs).
