# pitcrew-api

HTTP and WebSocket API over a unix socket or named pipe; composes every crate's routes.

**Owned by stream H** — see [docs/build/streams/H.md](../../docs/build/streams/H.md).

## Transport

| `Listen` | Where | Protection |
|---|---|---|
| `Unix { dir }` | `dir/pitcrewd.sock` | `dir` is created 0700; an existing one must already be ours and private (it is never re-permissioned). The socket is 0600; every connection's peer uid must equal the daemon's. A stale socket of ours is removed; a live one, another user's, or anything that is not a socket is left alone. |
| `Pipe { name }` | `\\.\pipe\pitcrewd-<user SID>` | A DACL granting only the current user; remote clients rejected. The daemon must create the name (first instance), so it never joins someone else's pipe. That does **not** stop another user creating the name while the daemon is down, so clients must check the server (below). |
| `DevTcp { addr }` | loopback only | **Development only**, never a default. Tokens are the only protection; a `Host` guard blocks DNS rebinding. |

`Listen::private_default(run_dir)` picks the socket or the pipe for the platform.

## Before a client sends a token

`pitcrew_api::client` has the checks the desktop and the CLI must make, so a socket or pipe
planted by another user never receives a token:

- Unix: `check_unix_socket(dir)` before connecting (directory ours and 0700, socket ours), and
  `check_unix_peer(&stream)` after (the server runs as us).
- Windows: `check_pipe_server(&client)` after connecting (the pipe is owned by the current
  user, or by our token's default owner). The daemon names the user as its pipe's owner; another
  user cannot create a pipe owned by us, and unlike a server process id, the owner cannot be
  recycled. The token's default owner is accepted so that a pipe created without naming an
  owner (a test server) passes: unelevated it is the user itself; elevated it is typically the
  Administrators group. Residual, unchanged by that: an elevated administrator (or anyone with
  the restore privilege) can plant a pipe that passes.

## Auth

- HTTP: `Authorization: Bearer <token>`. Query strings are never read.
- WebSocket upgrades may instead offer `Sec-WebSocket-Protocol: pitcrew.v1, pitcrew.bearer.<token>`.
  WebSocket handlers answer with `pitcrew_auth::WS_PROTOCOL` (`ws.protocols([WS_PROTOCOL])`).
- On success the token is removed from the headers and `Caller` is inserted as an extension. See
  [`pitcrew-auth`](../auth/README.md) for how routes read it.
- Failures are `ApiError` bodies: `401 unauthorized` (none, unknown, revoked, or not `Bearer`),
  `403 forbidden` (agent on a device route), `404 not_found` (unknown route or method).

## Live updates: `GET /v1/stream?since=`

`stream::routes(source, StreamConfig::default())`, mounted as a **device** route.

- The source is an `EventSource`: `StoreSource::new(store, log_id)` for the hub's store, or
  `MemorySource` for tests and development. `StoreSource` takes the log id from the caller until
  `Store::log_id()` lands (brief C-projections).
- First frame `hello {rev, log}`; if `since < rev`, the missed events in `events` frames of at
  most 500; then live batches, coalesced over 75 ms; `ping` every 20 s.
- Exact resume: the pump subscribes before reading `rev`, always reads from the last revision it
  sent, and skips announced ranges it already covered. A lagged subscription re-reads the latest
  revision and catches up.
- Backpressure: each client has a queue of `queue_frames` frames (default 64). A frame that
  cannot be queued within `send_timeout` (default 10 s) disconnects the client, which resumes
  with `since`.
- Close codes: **1013** too slow (resume with `since`), **1001** the event source closed or the
  hub is shutting down, **1011** the event source failed, **1009** a client message over 4 KiB.
  A client's Close is answered before the socket is dropped.

## Hooks: `POST /v1/hooks/{engine}/{event}`

`hooks::routes(HookIntake::start(sink, capacity)?)`, mounted as an **agent** route.

- `engine` must be an `Engine` (`claude`, `codex`, `opencode`); `event` must match
  `[A-Za-z][A-Za-z0-9_-]{0,63}`; the body must be a JSON object of at most 1 MiB. Anything else
  is `400 invalid`.
- The route answers `202` at once and queues a `HookEvent` (with the `Caller`) for the
  `HookSink`. When the queue is full the event is dropped and counted (`HookIntake::dropped`).
- `LogHookSink` only logs; the runner (stream D) provides the real sink. `deliver` runs on its
  own thread and may block (e.g. on SQLite). A panic in it loses that one event: it is logged
  with the panic's message, and **the sink keeps receiving** the next events.

## Terminals: `GET /v1/sessions/{id}/terminal?cols=&rows=&from=`

`terminal::routes(terminals, TerminalConfig::default())`, mounted as a **device** route.

- `Terminals` finds a session's terminal; `RuntimeTerminals::new(runtime)` implements it over
  any `Runtime`, with `link(session, terminal)` until the runner provides the mapping. Unknown
  sessions are `404`; an unreachable runtime is `503`.
- **The `Attachment` contract** (for whoever implements `Terminals`): every method may block but
  must return within bounded time, answering `Unavailable` rather than hanging; `exited()` may
  say `true` only once all output is readable; `NotFound` from `read` or `exited` during a
  stream means the program ended. `changes()` is an optional push hint (a `watch::Receiver`
  that changes on new output and on exit); without it the route polls.
- `cols` and `rows` are `terminal::SIZES` (1..=1000); outside it the query is `400` and a
  `resize` closes with 1007. The upgrade is checked (after the `404`) before anything touches
  the terminal, so a plain GET never resizes it.
- Output is binary frames of at most 64 KiB from `from` (default 0). If the buffer lost `from`,
  `{"type":"truncated","from":N}` comes first. A client counts the bytes it received and
  reconnects with `from=<that offset>`; nothing is lost or repeated.
- Idle cost: without `changes()`, polling backs off from 20 ms to 250 ms while nothing happens
  (one `read` and one `exited()` per round) and starts over on any input or output.
- Every call into the seam runs on the blocking pool, at most `max_calls` (64) at once across
  all clients, and is given up after `call_timeout` (5 s): `503` before the upgrade, 1011 after.
  A stalled runtime ties up at most `max_calls` threads. **The limit is global to the route, by
  design:** once `max_calls` calls hang on one runtime, calls for every other terminal of the
  route wait for a permit too (and time out with 503 or 1011) until the hung calls return. A
  limit per terminal would let a stalled runtime tie up threads without bound.
- Client binary frames are keystrokes; `{"type":"resize","cols","rows"}` resizes; other types
  are ignored; malformed control JSON closes with 1007, and a message over `max_inbound` (1 MiB)
  with 1009. When the program exits, or its terminal disappears, the rest of the output, then
  `{"type":"exit"}`, then close 1000.
- Input is written in order by its own loop, through a queue of 16 messages, so a terminal that
  is slow to take input holds up neither output nor pings. While the queue is full the socket is
  not read (back-pressure on the client); a write that times out closes with 1011.
- A client that stops reading is closed with 1013 and resumes by offset. So is one that does
  not answer the Ping sent every 20 s within 20 s. Several clients may attach; each gets the
  output, and their keystrokes interleave in arrival order.
- Close codes: **1000** after `exit`, **1007** malformed control, **1009** message too big,
  **1013** too slow or no Pong (reconnect with `from`), **1011** runtime failure, **1001** hub
  shutting down. A client's Close is answered before the socket is dropped.

## Shutdown

`Bound::serve` (and `serve`) tell every open WebSocket when `shutdown` completes; they close
with 1001, and `serve` waits up to 2 s for their closing handshakes to finish before returning.
Each socket holds its shutdown receiver until it has closed, which is how `serve` counts them.
So a `main` that returns as soon as `serve` does still closes its clients cleanly. Hyper's
graceful shutdown alone does not wait for upgraded connections.

## Activity: `GET /v1/events?before=&limit=&project=&workstream=&task=&session=`

`Activity::new(source).with_refs(refs).routes()`, mounted as a **device** route, on the same
`EventSource` as the stream. `refs` is the work model's activity index as an
`activity::EventRefs` (see "For the composition root"). `activity::routes(source)` is the same
route without an index.

- Oldest first within a page, the newest page without `before` (exclusive); `limit` defaults to
  100, max 500.
- The page is `pitcrew_protocol::api::EventsPage`. **Only `at_start` ends paging.**
- Filters combine: an event must match every one given.
- `session` and `task` match events with a `session` (or `task`) field, at any depth, holding
  the id or an object with that `id`; the id under another key (a `parent`, `blocked_by`, free
  text) does not match.
- With the index they also match what the index says an event is about, following the links in
  force when it happened: `task` adds the turns, tool runs and file edits of sessions linked to
  the task, and `dispatch_finished` and `ask_answered` of its dispatches and asks; `session` adds
  `dispatch_finished` and `ask_answered` of its dispatches and asks. **Without the index those
  are missed.**
- `project` and `workstream` match only through the index (events about the project or
  workstream, its tasks' and their sessions'), and answer `400 invalid` without one.
- Bounded work per request: `project` and `workstream` alone are answered by the index, which
  bounds its own search; a filter with `session` or `task` scans back at most 10,000 events and
  asks the index about each 500 it reads. Either way a page may be short or empty, with
  `at_start` false, `to_rev` 0 and `from_rev` where the search stopped.
- The route checks the index's answers (ascending, below `before`, at most `limit`, progress,
  revisions the log has) and answers `500` rather than a page that could make a client loop.

## Features

`store` (default) provides `StoreSource` and pulls in `pitcrew-store` (SQLite). Crates that
only need `pitcrew_api::client` can use `default-features = false`.

## For the composition root

```rust
let tokens: Arc<dyn TokenStore> = Arc::new(FileTokenStore::open(&state_dir)?);
let info = pitcrew_api::local_host_info(env!("CARGO_PKG_VERSION"), vec![HostRole::Hub, HostRole::Runner], caps);
let source: Arc<dyn EventSource> = Arc::new(StoreSource::new(store.clone(), log_id));
let hooks = HookIntake::start(Arc::new(LogHookSink), 1024)?;
let terminals = Arc::new(RuntimeTerminals::new(runtime.clone())); // the runner keeps it to link sessions
// `work` is the hub's one `Arc<WorkService>`; `WorkRefs` is below.
let refs: Arc<dyn pitcrew_api::EventRefs> = Arc::new(WorkRefs(Arc::clone(&work)));
let parts = RouterParts::new()
    .agent(pitcrew_api::hooks::routes(hooks))
    .agent(hub_work::agent_routes())    // routes marked **agent** in api-v1.md
    .device(pitcrew_api::stream::routes(source.clone(), StreamConfig::default()))
    .device(pitcrew_api::Activity::new(source).with_refs(refs).routes())
    .device(pitcrew_api::terminal::routes(terminals.clone(), TerminalConfig::default()))
    .device(hub_work::device_routes()); // everything else
pitcrew_api::serve(&Listen::private_default(run_dir)?, info, tokens, parts, shutdown).await?;
```

This crate does not depend on the work model, so the daemon adapts its index. The trait and the
filter mirror `pitcrew_hub_work`'s field for field:

```rust
/// The work model's activity index, as `pitcrew-api` takes it.
#[derive(Debug)]
struct WorkRefs(Arc<pitcrew_hub_work::WorkService>);

impl pitcrew_api::EventRefs for WorkRefs {
    fn revs_matching(
        &self,
        f: &pitcrew_api::RefFilter,
        before_rev: u64,
        limit: usize,
    ) -> Result<(Vec<u64>, u64), pitcrew_api::source::SourceError> {
        let filter = pitcrew_hub_work::RefFilter {
            project: f.project,
            workstream: f.workstream,
            task: f.task,
            session: f.session,
        };
        pitcrew_hub_work::EventRefs::revs_matching(&*self.0, &filter, before_rev, limit)
            .map_err(Into::into)
    }
}
```

Do not merge more routes into the router this builds: they would be unauthenticated. Put every
route in `RouterParts`.

Or `Bound::bind(&listen)` first (to report where it listens with `describe()`), then
`bound.serve(pitcrew_api::router(info, tokens, parts), shutdown)`.
