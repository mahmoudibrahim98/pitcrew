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
| `office.json` | Where the back office got to in the log (`{ "log", "done" }`), so a restart runs it again from there. Removed by `--no-office`. Private. |
| `run/pitcrewd.sock` | The private socket (Unix). On Windows the API uses the current user's named pipe, `\\.\pipe\pitcrewd-<user SID>`. |

On Unix the directory is created 0700, and an existing one must already be ours and private; on
Windows it must be under the user's profile, whose ACL it inherits.

### Start

1. The token registry, which takes the lock.
2. The store, with the work model's projections (`StoreOptions::default()`, whose `FsMode::Auto`
   picks the NFS-safe mode on a network filesystem).
   - The workspace is the demo's with `--demo` (written to `workspace.json`), else the one the
     store's events belong to, named by `workspace.json` when it names the same workspace and
     called "Workspace" otherwise. With `--demo`, a store with data is refused here.
   - The back office's member, `@office`, found or added (see "The back office"). The store is
     then opened again with the office's run log as well (`projections_with_office`), which
     needs that member; it catches up on open like any projection.
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
6. The back office's loop, the routes, the listener, and the ready line.

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
- otherwise the daemon reuses the workspace's agent `@office`, and on the first start without
  one appends a `member_added` for it (a new id, handle `@office`, name "Back office"), authored by
  the owner. It is appended before the hub's `WorkService` exists and before anything is served,
  so it races with no other writer;
- a workspace with no person yet has no owner for it: the office stays off (logged) until the
  first start after onboarding adds one. A *person* called `@office` also keeps it off.

Why an event at first start rather than seeding: the office's member is workspace data like any
other, so it belongs in the log, where every projection (and a rebuild) sees it; and the run log
must know the member before the store opens with it, so the daemon finds it (or adds it) first.
The member is created once and reused, because the run log's settings, the member included, must
stay the same for the life of the store.

**No token.** The office acts inside this process through the hub's `WorkService`
(`OfficeCommands`), so it has no token: none is minted, and nothing about it is written to the
state directory but `office.json`.

**The loop.** It subscribes to the store's appends before anything is served, reads the newest
revision straight away, and then runs `WorkService::run_office(last + 1 ..= to_rev)` on the
blocking pool for each batch, `last` moving forward only when a run succeeds. A failed range is
tried again with the next append, or after a wait that doubles from 1 s to 60 s. On a lag, the
upper bound is `latest_rev()`. Revisions another process appended are covered by the next range,
since it starts at `last + 1`. What the office appends is announced too, and looked at like any
batch. Each action applied is logged at info (`the back office acted rule=…`); each action the hub
refuses, as a warning (`the hub refused a back-office action`, from hub-work).

**Restarts.** `office.json` holds `last` for the store's log, rewritten at most once a second and
when the loop stops. The next start runs from there again: `run_office` is idempotent (an action
already in the log is `replayed` and appends nothing), so what a crash left unapplied is applied
then, and nothing twice. With `--demo` the office starts at revision 1, so it also looks at the
seed. Without `office.json` (the first start with the office on, or after `--no-office`) it starts
at the end of the log.

**On by default; `--no-office`.** The back office is part of what a hub does: the work moves on
its own when the evidence is in the log, which is what the desktop shows. Its actions are bounded
(the "never" list, caps per rule and per hour, the hub's own checks), signed by `@office`, and
undoable like any person's. `--no-office` turns it off: the store opens without its run log, and
`office.json` is removed, so what is appended meanwhile is never acted on later (the run log
catches up when the office is back on, but the office starts at the end of the log). Use it to
compare the hub with the mock hub, which has no office, or to debug the work model alone.

**The demo and the clock.** The demo's data stops on 2026-09-30; the office's clock is the newest
event's time. So the first write through the API at today's time makes the demo's open asks a day
or more old, and the office appends reminders (and, three days on, "paused?" proposals) right
after it, authored by `@office`.

## Routes

| Route | From |
|---|---|
| `GET /v1/host/info` (no token) | `pitcrew-api`; roles `["hub"]` until the runner is wired in |
| Work routes, agent and device, with `GET /v1/workspace` and `GET /v1/sessions[/{id}]` | `pitcrew-hub-work` (`agent_routes`, `device_routes`) |
| `POST /v1/tasks/{id}/dispatch` | `pitcrew-hub-work` without a dispatcher: `503 unavailable`, and nothing is recorded, not even an assignment |
| `GET /v1/stream` | `pitcrew-api` over the store (`StoreSource`) |
| `GET /v1/events` | `pitcrew-api`'s `Activity` over the store, with the work model's activity index (`with_refs`, through the `WorkRefs` adapter in `src/refs.rs`): `project=` and `workstream=` match events about them, their tasks and their sessions, and `task=` and `session=` also match their sessions' and dispatches' events |
| `POST /v1/hooks/{engine}/{event}` | `pitcrew-api`; logged at debug (engine, event, member; never the body) until the runner's sink exists |
| `GET /v1/sessions/{id}/terminal` | `pitcrew-api`; no runner yet, so `503 unavailable` for a known session, `404` for an unknown one |

On development TCP only, the daemon answers CORS as the mock hub does: preflights from
`http://localhost:<port>`, `http://127.0.0.1:<port>` and the Tauri app's origins get `204` and
`Access-Control-Allow-*`; other origins get `403`, and so do WebSocket upgrades from them.

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
request that changed this crate). The mock hub has no back office; with the daemon's on (the
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
  is logged as a warning, and appends nothing;
- a restart, plain or after a "crash" before `office.json` was saved, appends no office action
  twice;
- `GET /v1/events?project=…` and `?workstream=…` answer 200 with exactly the events about them
  (checked against the ids each event names), paged by 500 and by 1; filters combine; an unknown
  project is an empty page, a malformed id a 400, an agent a 403.

The unit tests in `src/serve.rs` cover adding `@office` once to a workspace that has a person
(and not before), `--no-office` removing `office.json`, and the loop over the demo across a
restart.

## Not wired yet

- The runner (stream D's hub link): its event sink, the hook sink, terminals, the dispatcher,
  agent tokens for real sessions, and the `SessionAgents` trait over hub-work's sessions and
  members; then `roles` gains `runner`.
- Creating the workspace and its first person outside `--demo`: there is no work command for it
  yet; the daemon would write `workspace.json` then. The back office's member is appended by the
  daemon itself for now, for the same reason.
- Remote machines, the Tauri shell, auto-start and installers.
