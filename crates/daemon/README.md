# pitcrew-daemon

`pitcrewd`, the composition root. For now it is the hub of a solo workspace: it opens the store,
runs the work model and its back office, and serves API v1 with real tokens. The runner,
terminals and remote machines join later (ADR-0009).

**Owned by stream 0**: see [docs/build/streams/0.md](../../docs/build/streams/0.md).

## Commands

| Command | What |
|---|---|
| `pitcrewd serve` | Serves API v1 on the private transport until a stop signal: Ctrl+C; SIGTERM or SIGHUP on Unix; Ctrl+Break or closing the console on Windows. |
| `pitcrewd serve --listen unix:<dir>/pitcrewd.sock` | Unix only: exactly that socket. The file name must be `pitcrewd.sock`; the directory is created 0700, or must already be ours and 0700. |
| `pitcrewd serve --listen tcp:127.0.0.1:<port>` | Loopback TCP, **for development only**: tokens are then the only protection. Non-loopback addresses are refused. |
| `pitcrewd serve --demo` | Seeds the demo workspace (`crates/fixtures`) first. Only into an empty store; a store with data is refused. |
| `pitcrewd serve --no-office` | Without the back office (see "The back office"). It is on by default. |
| `pitcrewd token show-path` | Prints where the device token is kept. Never the token. Fails if there is none yet. |
| `pitcrewd --version` | `pitcrewd 0.0.0 (protocol 1, oldest accepted 1)`: the bare version is the second word. Needs no state directory. |

**For launchers** (e.g. on a remote machine under tmux), `pitcrewd serve` runs in the
foreground and never daemonizes, so the pid you started is the daemon. It never reads stdin, logs
only to stderr, and prints one ready line on stdout. Any stop signal (SIGTERM, SIGINT, SIGHUP)
stops it gracefully: WebSockets close with 1001, the store closes, and the socket is removed.

`--state-dir <dir>` goes with any command (before or after it). Without it, the state directory
is the platform's local data folder, never a roaming or synced one: `%LOCALAPPDATA%\PitCrew\data`,
`~/.local/share/pitcrew` (or `$XDG_DATA_HOME/pitcrew`), `~/Library/Application Support/PitCrew`.

Logs go to stderr at the level in `PITCREW_LOG` (`tracing` directives: `debug`,
`info,pitcrew_api=debug`, …; default `info`). Tokens are never logged, at any level; tokens are
named by their id (`tok_…`) and token files by their path. Stdout carries one line once the
daemon is ready, `pitcrewd listening on <where>`, which supervisors and tests wait for.

## The state directory

| Path | What |
|---|---|
| `hub.db` | The hub's store: the event log and the work model's projections (SQLite, WAL). |
| `tokens.json` | The token registry: SHA-256 hashes only (`pitcrew-auth`). |
| `tokens.lock` | Held by the running daemon. **One daemon per state directory**: a second one stops at start with "another pitcrewd is already running on …". |
| `device.token` | The desktop's device token, `pcd_…`. Private (0600 on Unix). |
| `demo-agent.token` | With `--demo` only: a token for the demo's first agent, `@writer`, `pca_…`. Private. |
| `workspace.json` | The workspace's id and name (`GET /v1/workspace`), which the event log does not hold. Written by `--demo`. Private. |
| `office.json` | Where the back office got to in the log (`{ "log", "done" }`), so a restart runs it again from there. Removed by any start with the office off (`--no-office`, or no owner for `@office`). Private. |
| `run/pitcrewd.sock` | The private socket (Unix). On Windows the API uses the current user's named pipe, `\\.\pipe\pitcrewd-<user SID>`. |

On Unix the directory is created 0700, and an existing one must already be ours and private; on
Windows it must be under the user's profile, whose ACL it inherits.

### Start

1. The token registry, which takes the lock.
2. The store, opened once, with the work model's projections (`StoreOptions::default()`, whose
   `FsMode::Auto` picks the NFS-safe mode on a network filesystem).
   - The workspace is the demo's with `--demo` (written to `workspace.json`), else the one the
     store's events belong to, named by `workspace.json` when it names the same workspace and
     called "Workspace" otherwise. With `--demo`, a store with data is refused here.
   - The back office's member, `@office`, found or added (see "The back office"). The office's
     run log, which needs that member, is then registered on the open store
     (`Store::register(Box::new(back_office.run_log()))`); it catches up as any projection does at
     an open. The store is never closed and opened again, so on a network filesystem its
     single-host lease is held from the open until the store closes. When the office is off
     (`--no-office`, or no member it may act as), `office.json` is removed instead.
3. The store's one `WorkService` (hub-work's "one writer": everything shares that `Arc`).
   - The hub's own machine (`with_hub_machine`) is the workspace's first `local` machine (the
     demo's "This laptop"). Without one, a dispatch for a task with no folder answers 503.
   - No dispatcher until the runner link exists (see Routes).
4. With `--demo`: mint the tokens, then seed. Tokens come first, so a failure leaves the store
   empty and `--demo` can be retried.
5. The device token: `device.token` is reused while it verifies as a device token; otherwise a
   new one is minted for the workspace's first person (the demo's `@sam`) and written there. If
   the store has no person yet, the token acts as a new member that nothing knows, and
   `GET /v1/me` answers 404 until onboarding can add the person (see "Not wired yet").
6. The recap index's warm-up (see "Recaps"), started and not waited for; the back office's loop;
   the routes; the listener; and the ready line.

### Stop

Ctrl+C, Ctrl+Break, closing the console (Windows), or SIGTERM or SIGHUP (Unix): the server stops
accepting and finishes in-flight requests, `pitcrew-api` closes open WebSockets with 1001 (see its
README) and removes its unix socket, and meanwhile the back office finishes the run it is in and
saves `office.json` (`the back office stopped`). Then the store closes, checkpointing its WAL so
only `hub.db` remains, and the lock is released last. The log ends with `store closed` and
`stopped`.

## The back office

Stream F's office (`crates/office`), with its default rules and `Config`, applied by the work
model as hub-work's README ("Wiring", "The back office") describes. It moves a task to review when
its dispatch reports success, asks the owner about diverged runs and failing tests, reminds people
of asks left open for a day, and proposes "paused?" for quiet workstreams. Every action cites its
receipts, passes the office's "never" list (nothing outward, never done without automatic
acceptance, never a person's ask), and is checked again by the hub like any caller's.

**Its member, `@office`.** The office acts as an agent of the workspace, owned by the workspace's
owner (its first person, whom the device token also acts as):

- with `--demo` it is the demo's own `@office`, which the seed adds with everything else;
- otherwise the daemon reuses the workspace's `@office` when it is an agent of that owner, and on
  the first start without one appends a `member_added` for it (a new id, handle `@office`, name
  "Back office"), authored by the owner. It is appended before the hub's `WorkService` exists and
  before anything is served, so it races with no other writer;
- the office stays off (logged) when the workspace has no person yet to own it (until the first
  start after onboarding adds one), or when `@office` is a person, another person's agent, or an
  agent of no one: it never acts as a person, or for someone else.

Why an event at first start rather than seeding: the office's member is workspace data like any
other, so it belongs in the log, where every projection (and a rebuild) sees it; and the run log
must know the member before it is registered on the store, so the daemon finds it (or adds it)
first.
The member is created once and reused, because the run log's settings, the member included, must
stay the same for the life of the store.

**No token.** The office acts inside this process through the hub's `WorkService`
(`OfficeCommands`), so it has no token: none is minted, and nothing about it is written to the
state directory but `office.json`.

**The loop.** It subscribes to the store's appends before anything is served, reads the newest
revision straight away, and then runs `WorkService::run_office(last + 1 ..= to_rev)` on the
blocking pool for each batch, `last` moving forward only when a run succeeds. A run fails when
`run_office` does, or when the hub could not apply one of its actions for an internal reason
(`Internal` or `Unavailable`, say a full disk); a refusal by the rules (`Conflict`, `Forbidden`,
`NotFound`, `Invalid`, or the office's guard) is final and does not fail it. A failed range is
tried again after a wait that doubles from 1 s to 60 s, and appends meanwhile only extend it. On a
lag, the upper bound is `latest_rev()`. Revisions another process appended are covered by the next
range, since it starts at `last + 1`. What the office appends is announced too, and looked at like
any batch. Each action applied is logged at info (`the back office acted rule=…`); each action the
hub refuses, as a warning (`the hub refused a back-office action`, from hub-work).

**Restarts.** `office.json` holds `last` for the store's log. It is written when the office is set
up, before the listener binds (so a `--demo` start that then fails still gets its pass over the
seed next time), within a second of each run (at most once a second; a write that fails is tried
again a second later), and when the loop stops. The next start runs from there again:
`run_office` is idempotent (an action already in the log is `replayed` and appends nothing), so
what a crash left unapplied is applied then, and nothing twice. With `--demo` the office starts at
revision 1, so it also looks at the seed. Without `office.json` (the first start with the office
on, or after any start with it off, which removes the file) it starts at the end of the log.

**On by default; `--no-office`.** The back office is part of what a hub does: the work moves on
its own when the evidence is in the log, which is what the desktop shows. Its actions are bounded
(the "never" list, caps per rule and per hour, the hub's own checks), signed by `@office`, and
undoable like any person's. `--no-office` turns it off: the store opens without its run log, and
`office.json` is removed (as on any start with the office off), so what is appended meanwhile is
never acted on later (the run log catches up when the office is back on, but the office starts at
the end of the log). Use it to compare the hub with the mock hub, which has no office, or to debug
the work model alone.

**The demo and the clock.** The demo's data stops on 2026-09-30; the office's clock is the newest
event's time. So the first write through the API at today's time makes the demo's open asks a day
or more old, and the office appends reminders (and, three days on, "paused?" proposals) right
after it, authored by `@office`.

**Known gaps** (follow-ups, no code yet):

- *Internal failures look like refusals.* hub-work's `run_office` reports an action that failed
  for an internal reason (an append that hit a full disk, say) as that action's `Applied.result`,
  as it does a refusal by the rules, and returns `Ok`. The daemon tells them apart by the error's
  code (`Internal` or `Unavailable`: run the range again). Proposal for stream E: `run_office`
  returns `Err` at the first such failure, after the actions before it (which a re-run replays),
  so that every caller gets this right without classifying errors.
- *Who adds `@office`.* The `member_added` is authored by the workspace's person, as the
  bootstrap. A work-model command for adding members (stream E) would let the hub add it through
  its one writer instead of the daemon appending to the store.
- *A stale `office.json`.* If events were appended without the run log since `office.json` was
  written (by another process, or a build without the office), the next start runs the office
  over them, however old. A guard: before the run log is registered, and before anything is added,
  read its checkpoint (`projection_state` for `office.runs`); when it is behind the log, the
  revisions after it were appended while no office was watching, so start from the end of the log,
  as after `--no-office`. The tests that stand in for the runner link by appending from another
  process would then need the runner link itself.

## Routes

| Route | From |
|---|---|
| `GET /v1/host/info` (no token) | `pitcrew-api`; roles `["hub"]` until the runner is wired in |
| Work routes, agent and device, with `GET /v1/workspace` and `GET /v1/sessions[/{id}]` | `pitcrew-hub-work` (`agent_routes`, `device_routes`) |
| `POST /v1/tasks/{id}/dispatch` | `pitcrew-hub-work` without a dispatcher: `503 unavailable`, and nothing is recorded, not even an assignment |
| `GET /v1/stream` | `pitcrew-api` over the store (`StoreSource`) |
| `GET /v1/events` | `pitcrew-api`'s `Activity` over the store, with the work model's activity index (`with_refs`, through the `WorkRefs` adapter in `src/refs.rs`): `project=` and `workstream=` match events about them, their tasks and their sessions, and `task=` and `session=` also match their sessions' and dispatches' events |
| `GET /v1/recaps/blocks`, `GET /v1/recaps/days` | `pitcrew-api`'s `Recaps` over the hub's recap index (hub-work's `RecapIndex`, implemented by its `WorkService`), through the `WorkRecaps` adapter in `src/recaps.rs`; see "Recaps" |
| `POST /v1/hooks/{engine}/{event}` | `pitcrew-api`; logged at debug (engine, event, member; never the body) until the runner's sink exists |
| `GET /v1/sessions/{id}/terminal` | `pitcrew-api`; no runner yet, so `503 unavailable` for a known session, `404` for an unknown one |

On development TCP only, the daemon answers CORS as the mock hub does: preflights from
`http://localhost:<port>`, `http://127.0.0.1:<port>` and the Tauri app's origins get `204` and
`Access-Control-Allow-*`; other origins get `403`, and so do WebSocket upgrades from them.

## Recaps

`GET /v1/recaps/blocks` and `GET /v1/recaps/days` (api-v1, "Recaps"; device tokens only) are
answered by the hub's recap index: hub-work's `RecapIndex`, which its one `WorkService`
implements (hub-work's README, "Recaps"). Blocks and day paragraphs are derived from the log and
held in memory, never stored, so a restart builds them again.

- **The adapter** (`src/recaps.rs`, like `src/refs.rs` for the activity index): `pitcrew-api`
  does not depend on the work model, so `WorkRecaps` copies the route's `BlockFilter` field for
  field and its `DaysScope` variant for variant into hub-work's, passes `Some(limit)` (the route
  has applied the default and the cap) and `before.as_ref()`, and hands the index's error to the
  route, which answers `500`. The route validates everything the contract calls `400` first, so
  an `invalid` from the index means the two disagree about the contract: it is also logged as a
  warning (`the recap index refused a query the route had validated`).
- **Kept current on read.** Every query first reads the log from where the index got to, so the
  next query after a write (any writer's, the back office's included) shows it.
- **The warm-up.** After seeding (`--demo`) and before serving, `WorkService::sync_recaps()` runs
  once on the blocking pool, so the first request does not read the whole log itself. Start-up
  does not wait for it: a request meanwhile waits for the index, and finds it built. It logs
  `built the recap index rev=<the revision it reflects> ms=<how long>`, or a warning if it failed
  (then the first request goes on from where it stopped). With `--demo` it is a few milliseconds
  (67 revisions); a stop during a long warm-up waits for it before the store closes.
- **Any `tz`.** The daemon computes days at any offset from −840 to 840; the mock serves only
  `tz=0` from its fixture.
- **Not the mock's fixture.** The seeded demo's log is not the fixture's slice (the seed's own
  events are activity too), so the blocks and days differ from `demo-recaps.json`; hub-work's
  README, "The seeded demo is not the fixture", says how. The parity replay compares properties.

## Development: the UI against the daemon

The UI reads its API base URL from `VITE_PITCREW_API` and, in the dev server only, its token from
`VITE_PITCREW_TOKEN` (`apps/ui/src/data/config.ts`). Put the token there from the file; never in
a URL. PowerShell:

```powershell
cargo run -p pitcrew-daemon -- --state-dir $env:TEMP\pitcrew-dev serve --demo --listen tcp:127.0.0.1:47460
# In another terminal (the token file exists once the daemon has started):
$env:VITE_PITCREW_API = 'http://127.0.0.1:47460'
$env:VITE_PITCREW_TOKEN = Get-Content (cargo run -q -p pitcrew-daemon -- --state-dir $env:TEMP\pitcrew-dev token show-path)
corepack pnpm --filter @pitcrew/ui dev
```

bash:

```bash
cargo run -p pitcrew-daemon -- --state-dir /tmp/pitcrew-dev serve --demo --listen tcp:127.0.0.1:47460
VITE_PITCREW_API=http://127.0.0.1:47460 \
VITE_PITCREW_TOKEN="$(cat "$(cargo run -q -p pitcrew-daemon -- --state-dir /tmp/pitcrew-dev token show-path)")" \
  corepack pnpm --filter @pitcrew/ui dev
```

`--demo` works once per state directory; restart without it to keep the data, or use a new
directory for a fresh demo. The CLI takes the same token from the file:
`PITCREW_URL=http://127.0.0.1:47460 PITCREW_TOKEN_FILE=<that path> pitcrew …`.

## Parity with the mock hub

`parity/replay.mjs` replays the requests and assertions of `apps/mock-hub/test/http.test.ts`, and
the first test of each route in `edits.test.ts`, against the mock hub and against a real daemon
(fresh per test, `serve --demo`), and prints every check that differs. It needs Node 24, which
imports the mock's TypeScript:

```bash
cargo build -p pitcrew-daemon
node crates/daemon/parity/replay.mjs --pitcrewd target/debug/pitcrewd [--no-office] [--json report.json]
```

It is a report, not a test: some differences are expected (see the latest report in the pull
request that changed this crate). The recap routes are compared by property, not by value, since
the seeded demo is not the mock's fixture (see "Recaps"): order, paging, filters, spans, days
against blocks, and the `400`, `403` and `401` answers. Two recap rows differ by design: `tz=60`
(`400` on the mock, which serves `tz=0` only; `200` here) and the quiet workstream `WST0004`, whose
`workstream_created` the daemon's seed appends, so it has a day here and none in the fixture.
Paging loops are left out of the per-request status comparison (`record: false`), so the requests
after them still line up although the two servers hold different numbers of blocks. The mock hub has no back office; with the daemon's on (the
default), a check that reads the newest event right after a write may see `@office`'s reminders
instead, depending on timing. `--no-office` runs the daemon without it, to compare the hub alone.

## Tests

`tests/serve.rs` starts the real binary on a temporary state directory and a free port, and
covers `--version`, `token show-path`, tokens and scopes, the work routes (with the workspace,
sessions, and a dispatch that answers 503 and records nothing), the stream (a move
appears on it; a reconnect with `since` gets what it missed), hooks, terminals, CORS and the
`Host` guard, the single-daemon lock, and on Unix a SIGTERM stop: a clean store, the back office
stopped before it, `--demo` refused afterwards, a stream closed with 1001, and a restart that
keeps the token, the log and the data. Also on Unix: `--listen unix:<path>` binds exactly that
socket in a 0700 directory, and SIGTERM and SIGHUP both remove it (the back office stopping
first); `--version` creates nothing. Nothing it logs, at debug, holds a token. Its checks allow
for the back office appending after a write (the demo's asks are old by the wall clock).

`tests/office.rs`, with `--demo`, appends what the runner link will report straight to the store
(the daemon looks at it with its next append, here a comment through the API, or at its next
start):

- a dispatch that finishes moves its task to review, authored by `@office` on behalf of `@sam` as
  the back office; `@office` is an agent owned by `@sam`, and has no token;
- an action the hub refuses (the office's view is out of date after a racing writer's stale move)
  is logged as a warning for that revision, appends nothing, and is not run again;
- a restart, plain or after a "crash" before `office.json` was saved, appends no office action
  twice; a dispatch that finished while the daemon was stopped is acted on at the next start;
- a `--demo` start that seeds and then cannot listen still gets its pass over the seed at the
  next start;
- `GET /v1/events?project=…` and `?workstream=…` answer 200 with exactly the events about them
  (checked against the ids each event names), paged by 500 and by 1; filters combine; an unknown
  project is an empty page, a malformed id a 400, an agent a 403.

`tests/recaps.rs`, with `--demo`, checks what the contract promises of any log (the seeded demo
is not the mock's fixture):

- the warm-up logs the revision it built the index to, and how long it took;
- `GET /v1/recaps/blocks`: well-formed pages, newest first by id; every span of every line a
  non-empty slice of the text's UTF-8 bytes, on character boundaries, in order, with a receipt;
  paging by 1, 3 and 4 to `at_start` gives the whole list, never an empty page before the start;
  `before` the oldest block is an empty page; each filter (session, task, workstream, project)
  gives exactly the blocks carrying that link, and filters combine; an unknown id is an empty page,
  a prefixed lower-case id the same id; malformed ids and limits are `400`;
- `GET /v1/recaps/days`: newest date first, the entry without a workstream first within a date,
  then by workstream id; each entry covers blocks of its own workstream, and every block of the
  project is in exactly one entry; a workstream's days are its entries among its project's; paging
  by one date gives the whole; `tz=0` is the default, and at `tz` 60, −300, 840 and −840 the same
  blocks fall into days; malformed scopes, dates, `tz` and limits are `400`;
- an agent token gets `403` and no token `401`, on both routes;
- after a comment through the API, the very next blocks query has the block it begins, and today's
  entry of the task's workstream covers it.

The unit tests in `src/recaps.rs` check that the adapter copies every field and variant, passes
`Some(limit)` and `before`, and hands every error to the route, logging an `invalid` as a warning.
The unit tests in `src/serve.rs` and `src/office.rs` cover adding `@office` once to a workspace
that has a person (and not before); keeping the office off when `@office` is a person, another
person's agent or no one's agent; `--no-office` removing `office.json`; the start point saved
before the loop runs; the loop over the demo across a restart; a save that cannot write, tried
again until it can and at stop; which of the hub's failures are tried again; and, on a store
forced into network mode, its single-host lease taken once (the lease's clock is read once, at
acquisition) and held, the same file, while the office starts and acts, then let go of when the
store closes. (A lease let go of and taken again on a free path gets the same generation number,
so the number alone could not show it.)

## Not wired yet

- The runner (stream D's hub link): its event sink, the hook sink, terminals, the dispatcher,
  agent tokens for real sessions, and the `SessionAgents` trait over hub-work's sessions and
  members; then `roles` gains `runner`.
- Creating the workspace and its first person outside `--demo`: there is no work command for it
  yet; the daemon would write `workspace.json` then. The back office's member is appended by the
  daemon itself for now, for the same reason.
- Remote machines, the Tauri shell, auto-start and installers.
