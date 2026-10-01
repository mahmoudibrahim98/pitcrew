# pitcrew-daemon

`pitcrewd`, the composition root. For now it is the hub of a solo workspace: it opens the store,
runs the work model, and serves API v1 with real tokens. The runner, terminals and remote
machines join later (ADR-0009).

**Owned by stream 0**: see [docs/build/streams/0.md](../../docs/build/streams/0.md).

## Commands

| Command | What |
|---|---|
| `pitcrewd serve` | Serves API v1 on the private transport until a stop signal: Ctrl+C; SIGTERM or SIGHUP on Unix; Ctrl+Break or closing the console on Windows. |
| `pitcrewd serve --listen unix:<dir>/pitcrewd.sock` | Unix only: exactly that socket. The file name must be `pitcrewd.sock`; the directory is created 0700, or must already be ours and 0700. |
| `pitcrewd serve --listen tcp:127.0.0.1:<port>` | Loopback TCP, **for development only**: tokens are then the only protection. Non-loopback addresses are refused. |
| `pitcrewd serve --demo` | Seeds the demo workspace (`crates/fixtures`) first. Only into an empty store; a store with data is refused. |
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
| `run/pitcrewd.sock` | The private socket (Unix). On Windows the API uses the current user's named pipe, `\\.\pipe\pitcrewd-<user SID>`. |

On Unix the directory is created 0700, and an existing one must already be ours and private; on
Windows it must be under the user's profile, whose ACL it inherits.

### Start

1. The token registry, which takes the lock.
2. The store, with the work model's projections (`StoreOptions::default()`; `FsMode::Auto` once
   stream C's NFS work lands).
3. The workspace and the store's one `WorkService` (hub-work's "one writer": everything shares
   that `Arc`).
   - The workspace is the demo's with `--demo` (written to `workspace.json`), else the one the
     store's events belong to, named by `workspace.json` when it names the same workspace and
     called "Workspace" otherwise.
   - The hub's own machine (`with_hub_machine`) is the workspace's first `local` machine (the
     demo's "This laptop"). Without one, a dispatch for a task with no folder answers 503.
   - No dispatcher until the runner link exists (see Routes).
4. With `--demo`: refuse a store with data, then mint the tokens, then seed. Tokens come first,
   so a failure leaves the store empty and `--demo` can be retried.
5. The device token: `device.token` is reused while it verifies as a device token; otherwise a
   new one is minted for the workspace's first person (the demo's `@sam`) and written there. If
   the store has no person yet, the token acts as a new member that nothing knows, and
   `GET /v1/me` answers 404 until onboarding can add the person (see "Not wired yet").
6. The routes, the listener, and the ready line.

### Stop

Ctrl+C, Ctrl+Break, closing the console (Windows), or SIGTERM or SIGHUP (Unix): the server stops
accepting and finishes in-flight requests; `pitcrew-api` closes open WebSockets with 1001 (see its
README) and removes its unix socket; then the store closes, checkpointing its WAL so only `hub.db`
remains, and the lock is released last. The log ends with `store closed` and `stopped`.

## Routes

| Route | From |
|---|---|
| `GET /v1/host/info` (no token) | `pitcrew-api`; roles `["hub"]` until the runner is wired in |
| Work routes, agent and device, with `GET /v1/workspace` and `GET /v1/sessions[/{id}]` | `pitcrew-hub-work` (`agent_routes`, `device_routes`) |
| `POST /v1/tasks/{id}/dispatch` | `pitcrew-hub-work` without a dispatcher: `503 unavailable`, and nothing is recorded, not even an assignment |
| `GET /v1/stream`, `GET /v1/events` | `pitcrew-api` over the store (`StoreSource`). The `project=` and `workstream=` activity filters answer 400 until `pitcrew-api` takes hub-work's `EventRefs` |
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
node crates/daemon/parity/replay.mjs --pitcrewd target/debug/pitcrewd [--json report.json]
```

It is a report, not a test: some differences are expected (see the latest report in the pull
request that changed this crate).

## Tests

`tests/serve.rs` starts the real binary on a temporary state directory and a free port, and
covers `--version`, `token show-path`, tokens and scopes, the work routes (with the workspace,
sessions, and a dispatch that answers 503 and records nothing), the stream (a move
appears on it; a reconnect with `since` gets what it missed), hooks, terminals, CORS and the
`Host` guard, the single-daemon lock, and on Unix a SIGTERM stop: a clean store, `--demo`
refused afterwards, a stream closed with 1001, and a restart that keeps the token, the log and
the data. Also on Unix: `--listen unix:<path>` binds exactly that socket in a 0700 directory, and
SIGTERM and SIGHUP both remove it; `--version` creates nothing. Nothing it logs, at debug, holds a
token.

## Not wired yet

- The runner (stream D's hub link): its event sink, the hook sink, terminals, the dispatcher,
  agent tokens for real sessions, and the `SessionAgents` trait over hub-work's sessions and
  members; then `roles` gains `runner`.
- Activity filtered by `project=` or `workstream=`: `pitcrew-api`'s activity route does not take
  hub-work's `EventRefs` yet.
- Creating the workspace and its first person outside `--demo`: there is no work command for it
  yet; the daemon would write `workspace.json` then.
- Remote machines, the Tauri shell, auto-start and installers.
