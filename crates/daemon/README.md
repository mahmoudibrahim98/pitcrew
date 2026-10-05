# pitcrew-daemon

`pitcrewd`, the composition root. It is the hub of a solo workspace and its runner, in one process
(ADR-0009): it opens the store, runs the work model and its back office, watches this machine's
agent sessions (Claude Code, Codex, OpenCode) into the store, takes their hooks, and serves API v1
with real tokens. A fresh one is set up once, while it serves (see "The first run"). On a remote
machine, the same binary is the helper, reached over SSH (`pitcrewd connect`). Remote machines join
later.

**Owned by stream 0**: see [docs/build/streams/0.md](../../docs/build/streams/0.md).

## Commands

| Command | What |
|---|---|
| `pitcrewd serve` | Serves API v1 on the private transport until a stop signal: Ctrl+C; SIGTERM or SIGHUP on Unix; Ctrl+Break or closing the console on Windows. |
| `pitcrewd serve --listen unix:<dir>/pitcrewd.sock` | Unix only: exactly that socket. The file name must be `pitcrewd.sock`; the directory is created 0700, or must already be ours and 0700. |
| `pitcrewd serve --listen tcp:127.0.0.1:<port>` | Loopback TCP, **for development only**: tokens are then the only protection. Non-loopback addresses are refused. |
| `pitcrewd serve --demo` | Seeds the demo workspace (`crates/fixtures`) first. Only into an empty store; a store with data is refused. **Watches no agent home** unless `--homes` is given. |
| `pitcrewd serve --no-office` | Without the back office (see "The back office"). It is on by default. |
| `pitcrewd serve --homes <dir>…` | The runner watches these agent homes instead of this user's own (see "The runner"): `<dir>` is a folder laid out like a home folder (`<dir>/.claude`, `<dir>/.codex`, `<dir>/.local/share/opencode`); `claude=<dir>`, `codex=<dir>` or `opencode=<dir>` is one engine's home itself. Several may follow one `--homes`. |
| `pitcrewd serve --no-runner` | Without the runner: no session is watched, hooks are only logged (debug), and no session has a terminal or a transcript here (`503`). It is on by default. |
| `pitcrewd init --workspace <name> --name <person> --handle <@handle> --machine <name> [--listen <where>]` | Sets up the fresh workspace of the daemon running on this state directory (see "The first run"). `--listen` is where that daemon listens, as given to its `serve --listen` (default `private`). |
| `pitcrewd connect --socket <dir>/pitcrewd.sock [--framed] [--nonce <hex>]` | The stdio bridge to a daemon's socket, for a remote helper reached over SSH (see "`pitcrewd connect`"). Uses no state directory. Unix only. |
| `pitcrewd token show-path` | Prints where the device token is kept. Never the token. Fails if there is none yet. |
| `pitcrewd --version` | `pitcrewd 0.1.0 (protocol 1, oldest accepted 1)`: the bare version is the second word. Needs no state directory. |

**For launchers** (e.g. on a remote machine under tmux), `pitcrewd serve` runs in the
foreground and never daemonizes, so the pid you started is the daemon. It never reads stdin, logs
only to stderr, and prints one ready line on stdout. Any stop signal (SIGTERM, SIGINT, SIGHUP)
stops it gracefully: WebSockets close with 1001, the store closes, and the socket is removed.

`--state-dir <dir>` goes with any command but `connect` (before or after it). Without it, the
state directory is the platform's local data folder, never a roaming or synced one:
`%LOCALAPPDATA%\PitCrew\data`, `~/.local/share/pitcrew` (or `$XDG_DATA_HOME/pitcrew`),
`~/Library/Application Support/PitCrew`.

Logs go to stderr at the level in `PITCREW_LOG` (`tracing` directives: `debug`,
`info,pitcrew_api=debug`, …; default `info`). Tokens are never logged, at any level; tokens are
named by their id (`tok_…`) and token files by their path. Stdout carries one line once the
daemon is ready, `pitcrewd listening on <where>`, which supervisors and tests wait for; `init`'s
one line; and, for `connect`, only the bridge's bytes.

**For tests and development only**, `serve` takes hidden options (not in `--help`), each warned
in the log when used (see "Terminals"):

| Option | What |
|---|---|
| `--tmux-socket <path>` | The runner's tmux server on that socket instead of the state directory's own. |
| `--ptyd <path>` | That pitcrew-ptyd instead of the one next to `pitcrewd`. |
| `--ptyd-endpoint <path>` | pitcrew-ptyd on that endpoint (a socket path on Unix, a pipe name on Windows) instead of the state directory's own. |
| `--ptyd-idle-exit-ms <ms>` | A pitcrew-ptyd this daemon starts exits after that long idle (its own default is 30 seconds). |
| `--terminal-runtime pty` | The terminals run in pitcrew-ptyd even where tmux is usable (`auto`, the default, prefers tmux). |
| `--scan-hold-ms <ms>` | Each machine scan waits that long once accepted, holding its machine, before it walks: a second scan meanwhile can be shown to get `409` (see "The machine scan"). |

## The state directory

| Path | What |
|---|---|
| `hub.db` | The hub's store: the event log and the work model's projections (SQLite, WAL). |
| `tokens.json` | The token registry: SHA-256 hashes only (`pitcrew-auth`). |
| `tokens.lock` | Held by the running daemon. **One daemon per state directory**: a second one stops at start with "another pitcrewd is already running on …". |
| `device.token` | The desktop's device token, `pcd_…`. Private (0600 on Unix). |
| `demo-agent.token` | With `--demo` only: a token for the demo's first agent, `@writer`, `pca_…`. Private. |
| `workspace.json` | The workspace's id and name (`GET /v1/workspace`), which the event log does not hold. Written by `--demo`, and by the first run (`POST /v1/setup`, `pitcrewd init`). Atomic (a private file renamed into place). Private. |
| `office.json` | Where the back office got to in the log (`{ "log", "done" }`), so a restart runs it again from there. Removed by any start with the office off (`--no-office`, or no owner for `@office` yet, as before setup). Private. |
| `recaps.sqlite3` | The recap index's blocks (hub-work's README, "Recaps"): a cache, made when the index is built at start, replaced at every start and removed at a clean stop; never read from one run to the next. Private. On a network or unknown filesystem, kept in a private local fallback folder (temp before `$XDG_RUNTIME_DIR`), or memory if neither works; see "Recaps". |
| `runner/<log id>/` | The runner's index (`pitcrew-runner`): every transcript it watches, its session id, and how far it has been read into this store. One folder per hub log (the store's `log_id`), so a new store learns every session from the start. |
| `agents/<agent id>.token` | An agent token for each agent whose CLI the runner started (a dispatch's, or `POST /v1/sessions` with `agent`), bound to that agent and its owner, `pca_…`. The CLI is given its path (`PITCREW_TOKEN_FILE`), never the token. Minted once, reused while it verifies as exactly that. The folder is 0700, each file 0600. |
| `run/pitcrewd.sock` | The private socket (Unix). On Windows the API uses the current user's named pipe, `\\.\pipe\pitcrewd-<user SID>`. |

On Unix the directory is created 0700, and an existing one must already be ours and private; on
Windows it must be under the user's profile, whose ACL it inherits.

### Start

1. The token registry, which takes the lock.
2. The store, opened once, with the work model's projections (`StoreOptions::default()`, whose
   `FsMode::Auto` picks the NFS-safe mode on a network filesystem). The workspace is the demo's
   with `--demo` (written to `workspace.json`), else the one the store's events belong to, named
   by `workspace.json` when it names the same workspace and called "Workspace" otherwise (as
   before setup, when the store is empty). With `--demo`, a store with data is refused here.
3. The store's one `WorkService` (hub-work's "one writer": everything shares that `Arc`). It
   serves the name read in step 2 (the field `set_workspace_name` sets).
   - The hub's own machine (`set_hub_machine`) is the workspace's first `local` machine (the
     demo's "This laptop"). Without one (before setup), a dispatch for a task with no folder
     answers 503, and the runner stays off.
   - The setup listener (see "The first run").
   - Its dispatcher, the runner link over the runner attached here, which is empty until the
     runner starts (see "Dispatch"); and what the CLIs the runner starts for an agent get, its
     token's file (`AgentEnv`).
4. With `--demo`: mint the tokens, then seed. Tokens come first, so a failure leaves the store
   empty and `--demo` can be retried.
5. The device token: `device.token` is reused while it verifies as a device token; otherwise a
   new one is minted for the workspace's first person (the demo's `@sam`) and written there. If
   the store has no person yet, the token acts as a new member that nothing knows, and
   `GET /v1/me` answers 404 until the workspace is set up, which makes that member its person.
6. Unless `--no-office`, the back office (see "The back office"): its member, `@office`, found
   or added through the `WorkService`; then its run log, which needs that member, registered on
   the open store (`Store::register(Box::new(back_office.run_log()))`), catching up as any
   projection does at an open. The store is never closed and opened again, so on a network
   filesystem its single-host lease is held from the open until the store closes. When the
   office is off (`--no-office`, no person yet, or no member it may act as), `office.json` is
   removed instead.
7. The stop signals' handlers, so a stop from here on takes the clean path (see "Stop").
8. Unless `--no-runner`, the terminals' runtime (see "Terminals": tmux where it is usable, else
   pitcrew-ptyd where it is installed, else none, detected off the async executor), then the
   runner (see "The runner"). It reads the
   homes and writes into the store at once: the back office's first run covers what it appended,
   like anything else. A runner that cannot start does not stop the hub (see "The runner").
9. The recap index's warm-up (see "Recaps"), started and not waited for; the back office's loop;
   for a workspace without a person, the task that waits for its setup (see "The first run");
   the routes; the listener, which the agents' CLIs are told; the reconciliation of the sessions
   stored ahead of the runner (see "Dispatch"); and the ready line.

### Stop

Ctrl+C, Ctrl+Break, closing the console (Windows), or SIGTERM or SIGHUP (Unix): the server stops
accepting and finishes in-flight requests, `pitcrew-api` closes open WebSockets with 1001 (see its
README) and removes its unix socket. Meanwhile, as in the same step, the back office finishes the
run it is in and saves `office.json` (`the back office stopped`), and the runner stops: what it has
read is handed to the store first, its threads end, and with them its hold on the store (`the
runner stopped`). Each gets 10 seconds. This holds whether they started with the daemon or after
setup; one that a setup in flight starts once the stop has begun is stopped as it starts. Then,
once the runner has stopped, the terminals' runtime is let go of (`the terminals' runtime
detached`): tmux stores each terminal's exact output offset and its control client detaches, or
the connection to pitcrew-ptyd closes (ptyd keeps every terminal's output and offsets); then the
lock is released, while **the terminals keep running** (a stop never ends an agent; see
"Terminals"). Meanwhile the
store closes, checkpointing its WAL so only `hub.db` remains, and the lock is released last. The
log ends with `store closed` and `stopped`.

**A stop always ends, within about 20 seconds.** Whatever does not finish in time is left behind
and ends with the process, and the log says so (warnings): a runner stuck in a discovery or a read
on a filesystem that does not answer (`the runner is still stopping`); a transcript read past the
route's 15 seconds (`a transcript read has not returned`; the runner gives up on its own reads
after 10); a runtime that does not let go within 5 seconds (`the terminals' runtime is still
detaching`); anything that still holds the store 3 seconds after the server stopped; and, last,
work still on the blocking pool 5 seconds later (`work on the blocking pool is still running`).
The store is then closed with the process (`the store is still open at exit`), and its next open
recovers the log from its write-ahead file. Bounds: the 10 seconds of the stop step, then 5 for
the runtime alongside 3 for the store's holders, then 5 for the blocking pool.

## The first run

A fresh `pitcrewd serve` (no `--demo`, an empty store) has a device token but no person, no
machine and no name: `GET /v1/workspace` answers `setup_needed: true` (and the name
"Workspace"), `GET /v1/me` is `404`, host info has no `runner` role, and the back office is off.
It is set up once, with `POST /v1/setup` (api-v1.md, "The first run"), from the desktop's
onboarding or with `pitcrewd init`, and from then on works as a start with a person does, without a
restart:

1. hub-work's `set_up` appends the person (the device token's own member) and the machine, and
   serves the name. Under its writer lock it calls the daemon's listener (`src/setup.rs`,
   `Signal`), which only hands the result to a task over a channel: it never calls back into the
   `WorkService`, never writes, never blocks.
2. That task, once the lock is released, writes `workspace.json` (the id and the name; atomic,
   private, as `--demo` writes it), so a restart keeps the name; then names the hub's machine
   (`WorkService::set_hub_machine`), so a dispatch for a task without a folder may run here.
3. Unless `--no-office`, it starts the back office as step 6 of "Start" does: `@office` added
   through the writer (`ensure_office_member`), its run log registered, its loop from the end of
   the log (there is no `office.json`: the start without a person removed it).
4. Unless `--no-runner`, it starts the runner on the new machine, watching the homes settled at
   start (`--homes`, else this user's own; none with `--demo`, which never needs setup), and
   attaches it to the routes and host info.

The answer to `POST /v1/setup` comes once step 1 commits, so it may come a moment before steps
2–4 are done (api-v1.md says so): host info's roles show the runner once it runs. A part that
cannot start is logged and stays off until the next start, which starts it as any
start with a person does; the name is served even if `workspace.json` cannot be written (logged
as an error: a restart would then call the workspace "Workspace"). A second setup, or a racing
one, is a `409` and starts nothing. On every start the name comes from `workspace.json` (step 2
of "Start").

**`pitcrewd init`** (`src/init.rs`) is the same request for people without the desktop:
`pitcrewd [--state-dir <dir>] init --workspace <name> --name <person> --handle <@handle> --machine
<name>`. It asks the daemon running on that state directory, over its private transport (the
socket in `run/` on Unix, the user's pipe on Windows) or the `--listen` that daemon was started
with, with the device token from `device.token`, through the `pitcrew` CLI's client
(`pitcrew_cli::client`: its checks of the daemon before the token is sent, its HTTP, its errors). It
never opens the store and never prints the token. A handle may be given without its `@`
(PowerShell reads a bare `@sam` as something else). It prints one line naming the workspace, the
person and the machine; otherwise the reason on stderr, with the `pitcrew` CLI's exit codes:

| Exit | When |
|---|---|
| 0 | Set up. |
| 2 | The API's `400` (a name too long or blank once trimmed, a malformed handle, a control character), as its message. |
| 4 | The API's `409`: already set up, or the handle taken. |
| 5 | No daemon is running on this state directory ("no pitcrewd is running on … start it with `pitcrewd --state-dir … serve`"). |
| 1, 3 | Anything else, as the CLI says it (a daemon that fails the identity check, a token it does not accept). |

On Windows the private pipe is the user's own, not the state directory's: `init` reaches whichever
daemon holds it, and a daemon of another state directory refuses this one's token.

## `pitcrewd connect`

The remote end of the tunnel's stdio transport (`crates/remote`, "The stdio bridge"): on a remote
machine, the tunnel runs `pitcrewd connect --socket <path> [--framed] [--nonce <hex>]` over SSH for
each connection, and the bridge checks the socket (a private directory of this user's, a socket of
this user's, a listener running as this user) before it prints its ready mark and copies stdin to
the socket and the socket to stdout. The daemon's CLI hands everything after `connect` to
`pitcrew_remote::bridge::main` as it is, before anything else: no state directory is looked for or
made, nothing is logged, stdout carries only the bridge's bytes, and its errors go to stderr. The
exit codes are the bridge's: 2 usage (also clap's), 3 not this user's socket, 4 no daemon, 1
otherwise (and always on Windows, which has no unix sockets for it).

**What it costs.** `pitcrewd` ships as the static helper copied to remote machines, so its size
matters. Measured 2026-10-02, release build (`lto = "thin"`, stripped) for
`x86_64-unknown-linux-musl` with `cargo zigbuild`, as `packaging/build-release.sh --zig` builds it:

| `pitcrewd` | Bytes | Crates (normal dependencies, Linux) |
|---|---|---|
| Before this change | 12,042,144 | 137 |
| With `pitcrew-remote` alone (`connect`) | 12,082,576 (+40,432, +0.3%) | 144 |
| This branch: `connect`, `init` (`pitcrew-cli`'s client) and the first run | 12,329,648 (+287,504, +2.4%) | 146 |

`pitcrew-remote` brings seven crates: itself, and `toml_edit` (parsing only, for site recipes)
with `toml_parser`, `toml_datetime`, `winnow`, `indexmap` and `equivalent`; the same seven on
Windows. Everything else it uses (`tokio`, `rustix`, `getrandom`, `sha2`, `thiserror`, `serde`,
`windows-sys`) the daemon had already. Only the bridge is reachable from `pitcrewd`, so link-time
optimization leaves the rest of it (SSH, deploy, tunnels) out: 40 KB. `pitcrew-cli` adds itself and
`toml_writer` (its `toml_edit` with default features).

## The back office

Stream F's office (`crates/office`), with its default rules and `Config`, applied by the work
model as hub-work's README ("Wiring", "The back office") describes. It moves a task to review when
its dispatch reports success, asks the owner about diverged runs and failing tests, reminds people
of asks left open for a day, and proposes "paused?" for quiet workstreams. Every action cites its
receipts, passes the office's "never" list (nothing outward, never done without automatic
acceptance, never a person's ask), and is checked again by the hub like any caller's.

**Its member, `@office`.** The office acts as an agent of the workspace, owned by the workspace's
owner (its first person, whom the device token also acts as). The daemon finds or adds it through
the hub's one writer, hub-work's `WorkService::ensure_office_member(owner)`:

- with `--demo` it is the demo's own `@office`, which the seed adds with everything else;
- otherwise the workspace's `@office` when it is an agent of that owner, and when there is none,
  a new one (handle `@office`, name "Back office") in a `member_added` authored by the owner: at
  the first start with a person, or right after setup (see "The first run"), while the hub
  serves, which the command lock makes safe;
- the office stays off (logged) when the workspace has no person yet to own it (until it is set
  up), or when `@office` is a person, another person's agent, or an agent of no one (a `409` from
  `ensure_office_member`): it never acts as a person, or for someone else. Setup never makes
  `@office` a person's handle: it is reserved (api-v1.md, "The first run"), so on a hub set up
  here only data from elsewhere can hold it.

Why an event rather than seeding: the office's member is workspace data like any other, so it
belongs in the log, where every projection (and a rebuild) sees it; and the run log must know the
member before it is registered on the store, so the daemon finds it (or adds it) first. The member
is created once and reused, because the run log's settings, the member included, must stay the
same for the life of the store.

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
on, after setup, or after any start with it off, which removes the file) it starts at the end of
the log.

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
- *A stale `office.json`.* If events were appended without the run log since `office.json` was
  written (by another process, or a build without the office), the next start runs the office
  over them, however old. A guard: before the run log is registered, and before anything is added,
  read its checkpoint (`projection_state` for `office.runs`); when it is behind the log, the
  revisions after it were appended while no office was watching, so start from the end of the log,
  as after `--no-office`. The tests that stand in for the runner link by appending from another
  process would then need the runner link itself.

## The runner

Stream D's runner (`crates/runner`), joined to the hub in this process by its in-process link
(its README: `StoreSink`, `RunnerHooks`, `RunnerTerminals`, `SessionAgents`, the hook ownership
rule). `src/runner.rs` starts it; `--no-runner` leaves it off.

**Homes, and privacy.** Agent homes hold a person's private transcripts, so which are watched is
explicit:

- with `--homes`, exactly those;
- with `--demo` and no `--homes`, **none**: a demo never shows the person's real sessions. The log
  says `the runner watches no home (--demo without --homes)`;
- otherwise this user's own, as each adapter finds them (`pitcrew_ingest::scan::default_homes`):
  `CLAUDE_CONFIG_DIR` or `~/.claude`, `CODEX_HOME` or `~/.codex`, and `$XDG_DATA_HOME/opencode` or
  `~/.local/share/opencode` (on Windows too), where `~` is `USERPROFILE` on Windows (as the agents
  themselves read it), else `HOME`. The log lists them (`the runner watches these homes`). A home
  that does not exist yet is picked up when it appears.

Tests never watch a real home (see "Tests").

**Into the store.** The runner's `StoreSink` appends its batches to the store with `append_new`,
so a batch sent again after a crash is stored once; the work model's projections apply them in
the same transaction, and the stream announces them. Events of sessions without an agent are
authored by the workspace's first person, whom the device token acts as. The runner's sessions run
on the hub's own machine (the workspace's first local one). Without that machine, or without a
person, it stays off (logged), as the back office does without a person, until the workspace is
set up: then it starts on the new machine, watching the homes the daemon was started with,
without a restart (see "The first run"). Its index is `runner/<log id>/`.

**Reaching it.** The routes (hooks, terminals, session commands, transcripts, host info) reach the
runner through `runner::Attached`, set once when it starts, with the daemon or after setup; until
then hooks are only logged and terminals, session commands and transcripts answer `503`, as with
`--no-runner`.

**A runner that cannot start** (its index cannot be opened or is locked, one of its threads,
its terminals' included, cannot start) does not stop the hub: the daemon warns (`the runner cannot
start, so this hub serves without it`, with the reason and the index's folder) and serves as with
`--no-runner`. A desktop supervisor cannot pass `--no-runner`, and the hub's work is still worth
reaching.

**`GET /v1/host/info`** answers roles `["hub", "runner"]` while the runner runs, `["hub"]` when it
is off. Capabilities, while the runner runs: `tmux` when its terminals run in tmux or `pty` when
they run in pitcrew-ptyd (see "Terminals"), and `watch` while it watches at least one home (so
not with `--demo` alone), in that order; `[]` without a runner. They are read at each request, so
a runner that starts after setup shows at once: `pitcrew_api::router` answers the route with the
value it was built with, so the daemon answers `GET` and `HEAD /v1/host/info` itself in a layer
over the app (`src/host.rs`; `HEAD` with
the same headers, its length included, and no body) and passes every other request on, so the
router's fixed answer is never served.

**Hooks, and `SessionAgents`.** `POST /v1/hooks/{engine}/{event}` goes through the API's
`HookIntake` to `RunnerHooks`, which applies a hook only when its sender may change the session
(the runner README's rule: an agent token, its own sessions; a person, sessions without an agent
or of an agent they own; anything else, and an unknown agent, refused). Who runs a session comes
from `HubAgents` (`src/agents.rs`), the runner's `SessionAgents` over hub-work's sessions and
members:

- **Current by construction.** Each answer is one `Store::read` (a fresh read transaction) of the
  hub's `work_sessions` and `work_members`, with no cache. It sees every write committed before
  the question: the hub's commands, the runner's `StoreSink`, another process. Nothing has to tell
  it that a session gained an agent, so it can never answer "no agent" from a stale copy.
- A session stored with an agent answers that agent and its owner; one stored without, or not
  stored at all, "no agent" (the runner states every session without one, and a dispatch stores
  its agent before its CLI starts).
- **A sub-agent runs as its parent:** the answer is the agent found up the chain of `parent`s,
  for at most 16 sessions, the session itself included. Two sessions of a chain naming different
  agents, a chain going on past 16 sessions (to a 17th, stored or not: erring safe) or one that
  loops, an agent the hub does not know as an agent member, or a read that fails (warned once,
  then logged at debug): **`Unknown`**, and the runner refuses the hook.
- It reads the store only, never the runner, so it cannot wait for the watcher thread that asks.
- **A window at discovery.** The runner decides the hooks it held for a new session when it
  discovers it, before the hub has stored the session. Its `parent` cannot be seen then, so a
  sub-agent answers "no agent" even when its parent has one; the runner then asks about the
  parent itself (`agent_of(parent)`), so a dispatched agent's held hooks for its sub-agents are
  judged by that agent.
- **Whose hooks apply** (the runner README, "Session ids, and sessions the hub named"): a session
  the runner found on its own has no agent in the hub, so the person's hooks (device token)
  change it and an agent token's do not. A dispatched session (or one started for an agent) runs
  as its agent from the start: the runner reports its CLI under the id the hub stored with the
  agent, and the CLI's hooks carry that agent's token (see "Dispatch"), so the agent's own hooks
  change it, and its owner's.

**Transcripts.** `GET /v1/sessions/{id}/transcript?before=&limit=` (api-v1, "Transcript paging")
is served by `src/transcripts.rs` from the runner's own pages (`RunnerTranscripts`, its README's
"Transcript pages"): the runner finds a session's transcript by its id among those it watches,
and reads the page with the adapter's own `read_page`, on its own two threads, giving up after 10
seconds (a busy pool refuses at once). The route keeps what only the hub knows:

- the query: `limit` 200 by default, 1000 at most; a `limit=0`, or a `before` or `limit` that is
  not a whole number, `400`;
- an unknown session `404`; another machine's, or any without a runner, `503`;
- a session of this machine the runner has not indexed (the demo's, or a dispatched one whose
  transcript does not exist yet) an empty page with `at_start: true`, as the mock answers for a
  session without a transcript;
- a transcript that is gone (deleted) or cannot be read, and a read that is busy or does not end
  in time, `503 unavailable`, with the runner's reason (never a path). The runner logs the first
  failure for a session as a warning, later ones at debug.

The call runs on the blocking pool with the route's own 15 seconds as a backstop; since the runner
bounds its reads, a filesystem that does not answer ties up the runner's two page threads, not a
tokio thread per request.

## Terminals

The runner's terminals (`RunnerTerminals`, over `src/runtime.rs`'s `TerminalRuntime`) are where
the sessions PitCrew starts run.

**The runtime, chosen at start** (unless `--no-runner`): **tmux where it is usable, else
pitcrew-ptyd** (the PTY runtime), else none. `src/runtime.rs` does it in this order, every step off
the async executor:

1. The tmux socket's directory is made private (0700 if missing; one that is not is refused, never
   repaired), and its lock (`<socket dir>/lock`, `flock`) is taken. A lock another daemon holds
   means no tmux (`another pitcrewd uses this tmux socket`), before tmux is asked anything.
2. If pitcrew-ptyd is installed (see "Where pitcrew-ptyd is"), its endpoint's directory is made
   private the same way. A missing ptyd makes nothing.
3. `pitcrew_runtime::choose_async(TmuxOptions, PtyOptions)` checks, on a thread of its own, that
   tmux is installed (an absolute path on `PATH`), 3.2 or newer, and can start a server on that
   socket; if not, that pitcrew-ptyd is a program file where it is looked for and that its
   endpoint is safe (nothing is started).
4. For tmux, a server already running there must have no sessions but PitCrew's own (`pitcrew`):
   one with others (a person's own tmux, if a socket names it) is refused and left as it is, and
   the PTY runtime is tried instead.
5. The runtime is built with `Chosen::into_runtime`, behind the daemon's own wrapper that lets go
   of it at stop. For the PTY runtime the tmux lock is let go of, and the endpoint's lock taken (the
   same file when both are in one directory, as by default).

In tmux, the terminals are windows of that server (`TmuxRuntime`: its README has the details), the
log says `the runner's terminals run in tmux; attach to them with tmux -S <socket> attach -t
pitcrew`, and host info reports `tmux`. In pitcrew-ptyd (`PtyRuntime`), the log says `the runner's
terminals run in pitcrew-ptyd, which keeps them running when pitcrewd stops; tmux is not used:
<why>` at info, with the ptyd it runs, its endpoint and (Unix) its log, and host info reports
`pty`. Nothing is started yet: the runtime starts ptyd, detached, with the first terminal, and
ptyd exits by itself once it has had no terminal and no client for 30 seconds.

Otherwise there is no runtime (`NoRuntime`): no session has a terminal here, starting one is
`503`, and the log says why for both as a warning, `the runner's terminals cannot use tmux or
pitcrew-ptyd, so no session has a terminal here: <tmux's reason>; <ptyd's reason>`. tmux: not
installed, too old, the socket's directory refused, `another pitcrewd uses this tmux socket`, a
server with sessions `that are not PitCrew's`, or not Unix. ptyd: `pitcrew-ptyd is not installed
at <the path it was looked for at>`, its endpoint refused, or `another pitcrewd uses this
pitcrew-ptyd endpoint`.

**Where pitcrew-ptyd is.** Next to the running `pitcrewd`, in the same directory
(`pitcrew-ptyd.exe` on Windows), and **never on `PATH`**: the daemon runs only the ptyd it ships
with, built with the same protocol (stream P bundles it; see `crates/ptyd`'s README). A ptyd of
another protocol already running on the endpoint is refused at its first call, with a message, and
its terminals keep running.

**One tmux server and one ptyd per state directory.** The socket is
`<dir>/<8 hex digits of the sha256 of the canonical state directory>/tmux`, where `<dir>` is the
runtime's private per-user directory: `$TMUX_TMPDIR/pitcrew-<uid>` or `$XDG_RUNTIME_DIR/pitcrew`
when that is a private directory, else `/tmp/pitcrew-<uid>` (also when the path would pass the
103-byte socket limit). ptyd's endpoint follows the same rule: on Unix it is `ptyd` next to the
socket, in the same directory and under the same lock (ptyd keeps its own `ptyd.lock` and its log,
`ptyd.log`, there too); on Windows it is the user's pipe with the same digits,
`\\.\pipe\pitcrew-ptyd-<user SID>-<8 hex digits>` (`…-<SID>-elevated-<8 hex digits>` for an
elevated daemon). So two daemons of one user (a real one and a demo, a development one, another
state directory) never share a tmux server or a ptyd: not their terminals, not their offsets, not
the environment their programs inherit (the daemon that starts a server or a ptyd gives it its
own `HOME`, `PATH` and the rest). On Unix the lock keeps a second runtime off one socket or endpoint
even when one is named twice; on Windows no lock is taken beyond the state directory's own
(`tokens.lock`), which the default pipe, named after it, already follows. The per-user directory
depends on the environment, so a daemon restarted in another one (say without
`XDG_RUNTIME_DIR`) does not find its terminals.

**For tests and development**, the hidden `serve` options (see "Commands") name another socket
(`--tmux-socket`), another ptyd (`--ptyd`) or endpoint (`--ptyd-endpoint`), a shorter idle exit
(`--ptyd-idle-exit-ms`), or force the PTY runtime where tmux is usable (`--terminal-runtime pty`),
each with a warning. Tests always pass `--tmux-socket` and `--ptyd`, or point the per-user
directory at their own folder (`TMUX_TMPDIR`).

**Deploying pitcrewd with pitcrew-ptyd.** ptyd outlives the daemon only if what runs the daemon
lets it:

- **systemd.** A unit's default `KillMode=control-group` kills every process left in the unit's
  control group when the unit stops or restarts, ptyd and its terminals included: ptyd leaves the
  daemon's session (`setsid`), not its control group. A unit that runs `pitcrewd` must set
  `KillMode=process` (or `mixed`), or ptyd must run in a unit or scope of its own; otherwise every
  stop of the daemon ends every agent. (A tmux server the daemon started is in the same control
  group, and ends the same way.)
- **Windows Job Objects.** The runtime starts ptyd outside any job `pitcrewd` is in
  (`CREATE_BREAKAWAY_FROM_JOB`). A job that does not allow breakaway
  (`JOB_OBJECT_LIMIT_BREAKAWAY_OK`) refuses that, so ptyd starts inside it (logged as a warning)
  and ends when the job is closed, with every terminal; a job with `KILL_ON_JOB_CLOSE` (as many
  supervisors and terminals use) closes with its last handle. Whatever starts `pitcrewd` in a job
  (the desktop app, a service wrapper) must allow breakaway.

**Attaching by hand** (tmux only): `tmux -S <socket> attach -t pitcrew` (the socket is in the start
log), then pick a window (each is named after the CLI and the folder, e.g. `claude paper`); or one
terminal, `tmux -S <socket> attach -t pitcrew:@<n>`. Detach with `C-b d`. tmux's own key bindings
apply; the user's `~/.tmux.conf` does not, and the user's own tmux server is never touched.
Nothing attaches to a terminal of pitcrew-ptyd by hand: its terminals have no `native_target`,
and are reached through the API only.

**`GET /v1/sessions/{id}/terminal`** is answered by `SessionTerminals` (`src/terminals.rs`): a
session the hub does not know is `404`; one on another machine `503` (no remote runners yet); one
on this machine is `RunnerTerminals`' to find, `404` while it has no terminal (a session PitCrew
did not start, or any without a runtime). Every runtime call goes through `RunnerTerminals`, which
runs it on the runner's own small pool, bounded (5 seconds; starting a program 30), and the route
calls the seam on the blocking pool, bounded too. Nothing in the daemon calls `screen()` yet; it
would go the same way (the runtime's README: call it from a blocking thread).

**Session commands** (`src/sessions.rs`, device routes) are run by the runner's `RunnerCommands`:

| Route | What |
|---|---|
| `POST /v1/sessions` | Starts the CLI (`engine`) in a new terminal in `cwd` (see below), with `brief` (at most 64 KiB), `model` and `permission_mode`; `202` with the session. |
| `POST /v1/sessions/{id}/send` | Types `text` (at most 64 KiB), then Enter. |
| `POST /v1/sessions/{id}/keys` | Sends `keys` (1 to 64). |
| `POST /v1/sessions/{id}/interrupt` | Escape. |
| `POST /v1/sessions/{id}/end` | `graceful`: Ctrl-C twice, then waits up to 10 seconds for the CLI to exit; `kill`: SIGTERM to its process group, SIGKILL half a second later, and the window closes. Either reports the session ended (`session_ended`). |

- **Starting.** The runner learns a session's id only when the CLI writes its transcript; Claude
  is started with a session id of the runner's choosing (`--session-id`), so the match is exact,
  and other CLIs are matched by folder and start time. So `POST /v1/sessions` starts the CLI,
  waits up to 30 seconds for the runner to discover its session in that terminal (asking it to
  look again after 1, 3, 7 and 15 seconds), and answers `202` with the session as the hub stores
  it (`terminal` set). If it does not appear (Claude writes its transcript at its first prompt, so
  a start without a brief waits for a person), it answers `503` saying so: the CLI keeps running
  in its terminal, and its session appears on the stream once its transcript does.
- **With `agent` or `task`** the hub stores the session first (state `starting`, the agent
  named, linked to the task with `link_basis: manual`; `400` for an unknown agent or task, or a
  person as the agent; `403` for an agent the caller does not own), and the runner starts the CLI
  under that id, which its transcript adopts, as for a dispatch (see "Dispatch"): the start
  answers `202` with the session as stored once the CLI has started, and the CLI of a session run
  as an agent gets that agent's token file. If the runner refuses or fails, the session ends and
  the error is answered; if it has not answered in time, the start answers `503` and the session
  stays `starting` until it does (a refusal or failure then ends it). A Codex or OpenCode start
  in a folder where one started for an agent or a task (or a dispatch's) still waits for its
  transcript is refused (`400`): the two could not be told apart.
- `machine` must be a machine of the workspace (`400`) and the runner's (`503` for another).
  `persona` is passed on (the runner does not use it yet); `bypass_permissions` is refused by the
  runner (`400`), and so is a session id or model that could be read as an option.
- **`cwd`**: absolute, at most 4096 bytes, an existing folder; resolved once by the daemon (links
  and `..`), and the CLI starts in the resolved folder. On Unix it and every folder above it must
  belong to root or this user, and none may be writable by every user (o+w), except a sticky
  folder above it (as `/tmp`): anyone who can write there could plant files the CLI reads as its
  project's (settings, hooks, instructions) or swap the folder. Refused is `400`, saying which
  folder. Anything on a Windows drive mounted in WSL without metadata (`/mnt/c`, mode 777) is
  refused too, with no exception. **Group-writable folders are allowed** (a project shared with a
  group, as on a cluster), the folder itself or any above it; a start in one is logged at info (`a
  session starts in a folder that members of its group can change`), naming them, since the
  group's members can change what the CLI reads there.
- **Who may.** A person may command a session with no agent, or one whose agent they own; a
  session of another person's agent, or of an agent the hub does not know, is `403`. It is the
  runner's rule for hooks (its README), asked of the hub's tables (`HubAgents`).
- An unknown session is `404`; one on another machine, or any without a runner, `503`; an ended
  one, or one without a terminal here, `409`. A command the runner refuses is `400`; one that
  fails (the runtime cannot start or reach the terminal, or does not answer in time) is `503`.
- **Bounds.** A body is at most 1 MiB (`400 invalid` past it). Commands run on the blocking pool,
  at most 16 at once, and starts at most 4 at once, each holding its place through its wait for
  the session (`503` past either). A command's place is given back only when the command returns,
  even after the request stopped waiting for it (45 seconds, over the runner's own timeouts).
  Every lookup (the session, its agent, its terminal, the folder) is bounded by 5 seconds (`503`).
  While a start waits, it looks the session up by its terminal in the runner's index, every
  200 ms, not through the hub's whole list.

**Restarts.** A stop lets go of the runtime only after the runner has stopped; the terminals and
their programs keep running, and the runner's index still links each to its session.

- **tmux:** dropping `TmuxRuntime` stores each terminal's exact output offset in tmux
  (`@pitcrew-offset`) and detaches. The next start's runtime finds the terminals again by their
  tags, and output is numbered on from the stored offset: a client that reconnects with
  `from=<the offset it had>` misses nothing printed since, and one from `0` is told (`truncated`)
  where the kept output begins. Output printed while no daemon was attached is not in the stream
  (tmux sends only live output); the terminal's screen shows it to a person who attaches.
- **pitcrew-ptyd:** dropping `PtyRuntime` closes its connection; **offsets live in ptyd**, which
  keeps reading every terminal while no daemon is connected. The next start's runtime connects
  again and `list()` finds every terminal by its id: a client that reconnects with `from=<the
  offset it had>` gets everything printed since, what was printed while no daemon ran included,
  and one from `0` the whole history ptyd keeps (2 MiB a terminal; `truncated` only past that).

`end` ends a terminal; nothing else does.

## Dispatch

`POST /v1/tasks/{id}/dispatch` starts the agent's CLI on this machine, as a session already linked
to the task (`src/dispatch.rs`; hub-work's README, "Dispatch", for what is recorded):

- **The dispatcher** is `RunnerLink`, hub-work's `Dispatcher` over the runner's `RunnerCommands`.
  It is built over the [`Attached`] runner, which `open_with` now makes before the `WorkService`
  (rather than adding a late `set_dispatcher` to the service): the service's dispatcher is fixed
  when the one service is made and shared, and it reaches the same runner the session routes
  reach, attached at start or once the workspace is set up. Without one attached (before setup,
  `--no-runner`, a runner that could not start), or for a machine other than the runner's, a
  dispatch answers `503` with the reason, after its own `404`, `400`, `403` and `409`, and
  nothing is recorded. A person may dispatch only an agent they own (hub-work's `403`).
- **Starting** runs the dispatch's `StartSession`, which names the dispatch's session: the runner
  records the terminal under it at once and has the CLI's transcript adopt it (Claude by the
  `--session-id` it chose, Codex and OpenCode by folder and start time), so the session appears
  once, under the dispatch's id, linked to the task, with its terminal. The folder (`~` is this
  user's home) is resolved and checked as `POST /v1/sessions` checks a `cwd` (see "Terminals").
  The runner refusing (a folder that is not one, a permission mode it does not allow, a second
  Codex or OpenCode start in a folder where one for a named session still waits) is `409`; a start
  that fails (no terminal runtime, one that does not answer) is `503`. Either way the dispatch is
  finished as failed and its session ended.
- **The CLI's token.** The runner gives the CLI of a session the hub stored the environment
  `AgentEnv` (the runner's `SessionEnv`) returns: for a session run as an agent, `PITCREW_TOKEN_FILE`
  is `agents/<agent id>.token` in the state directory, a private file holding an **agent** token
  bound to that agent and its owner (minted once, reused while it verifies as exactly that), and
  `PITCREW_SOCKET` (or `PITCREW_PIPE`, or `PITCREW_URL` on development TCP) says where this daemon
  listens. The person's device token is never given, and no token is put in the environment
  itself (tmux keeps a program's variables). **Nothing inherited wins over these**: ptyd and the
  daemon's tmux server pass the daemon's own environment on, and the CLI reads `PITCREW_TOKEN`
  before `PITCREW_TOKEN_FILE` (and the platform's endpoint before `PITCREW_URL`), so a daemon
  started from a shell exporting a person's token would otherwise hand it to every agent.
  `PITCREW_TOKEN` and the endpoint variables the daemon does not listen on are set empty, which
  the CLI reads as unset. An agent without an owner, or a session whose agent is not known, is
  not started. A session without an agent gets nothing.
- **The task moves itself.** The runner's `StoreSink` is wrapped in `FollowingSink`: every batch
  the store takes is handed to `WorkService::follow_sessions`. The dispatched session's first
  `working` moves its task to in progress; its end finishes a dispatch still open (`canceled`,
  or `failed` if never reported). The agent's report (it moves the task to review) finishes the
  dispatch as `succeeded`, and the back office's `dispatch_to_review` covers a task left in
  progress.
- **Reconciling** (`reconcile`, a task of the daemon): at start, when the runner attaches, and
  after each start of a session the hub stored ahead of the runner, it looks at the sessions on
  the runner's machine still `starting` with no CLI id, plus sessions with active dispatches
  after adoption (`reconciling_sessions`), then again with
  a pause growing from 1 s to a minute while any waits. A start still under way is left alone,
  however long the runner takes: this hub's own (`Attached::starting`, held by the dispatcher and
  by `POST /v1/sessions` from before the runner is asked until it answers) is not even asked
  about, and the runner answers its own as `Running` until its command returns. One whose CLI
  runs, or whose transcript the runner has, is left to be reported. One the runner has no
  terminal and no transcript for (a crash between the dispatch and its start), or whose program
  ended before its transcript appeared, twice in a row with a rescan between, is abandoned: its
  dispatch fails ("its CLI did not start here, or ended before its transcript appeared"), it
  ends, and the runner retires its terminal (`RunnerCommands::retire`), so no later transcript
  is taken for it. So is a Codex or OpenCode session whose transcript has not appeared within
  the runner's 15-minute claim window (`Started::TooLate`), at once: none can be matched to it
  any more; its CLI is left running in its terminal. Before calling an exited unreported CLI
  gone, the runner scans existing transcripts for adoption. `Started::Exited` keeps waiting
  until that adoption reaches the hub; after it does, terminal exit ends the session and cancels
  the dispatch without an end hook. A report that finished the dispatch meanwhile keeps its
  outcome. With no runner attached it waits.
- **`POST /v1/sessions` with `agent` or `task`** goes through the same adoption: the hub stores
  the session first (`record_start`; `403` for an agent the caller does not own), then the CLI
  starts under its id (see "Terminals"). A start the runner has not answered within
  `COMMAND_TIMEOUT` (45 s) is answered `503` and the session left `starting`; a refusal or a
  failure that comes later still ends it.

The queue, per-agent concurrency, hand-off, and runners on other machines (over the JSON-lines
link) are not part of this.

**Board drafts** (api-v1.md, "Board drafts"; hub-work's `crate::board`) start their agent's CLI
the same way: `RunnerLink::start_session` runs the draft's `StartSession` under the session the
hub stored (state `starting`, the agent named), in the workstream's folder checked as above, and
`AgentEnv` gives that CLI the agent's token file, so the agent answers with `pitcrew board submit`
as itself. A refusal or a failure ends the session (and so the draft); the reconciliation looks
after a draft's session as after any the hub stored ahead of the runner. `serve` mounts hub-work's
`board_agent_routes` (the proposal) and `board_device_routes` (preview, start, list, review).

## Routes

| Route | From |
|---|---|
| `GET /v1/host/info` (no token) | `src/host.rs`, a layer over `pitcrew-api`'s app (see "The runner"); roles `["hub", "runner"]` while the runner runs, else `["hub"]`; capabilities, while it runs, `tmux` or `pty` for where its terminals run and `watch` while it watches a home, else `[]`; read at each request |
| Work routes, agent and device, with `GET /v1/workspace`, `POST /v1/setup` and `GET /v1/sessions[/{id}]` | `pitcrew-hub-work` (`agent_routes`, `device_routes`); setup's listener is the daemon's (see "The first run") |
| `POST /v1/tasks/{id}/dispatch` | `pitcrew-hub-work`, with `src/dispatch.rs`'s `RunnerLink` as its dispatcher (see "Dispatch") |
| `GET /v1/stream` | `pitcrew-api` over the store (`StoreSource`) |
| `GET /v1/events` | `pitcrew-api`'s `Activity` over the store, with the work model's activity index (`with_refs`, through the `WorkRefs` adapter in `src/refs.rs`): `project=` and `workstream=` match events about them, their tasks and their sessions, and `task=` and `session=` also match their sessions' and dispatches' events |
| `GET /v1/recaps/blocks`, `GET /v1/recaps/days` | `pitcrew-api`'s `Recaps` over the hub's recap index (hub-work's `RecapIndex`, implemented by its `WorkService`), through the `WorkRecaps` adapter in `src/recaps.rs`; see "Recaps" |
| `POST /v1/machines/{id}/scan` | `src/scan.rs`, a device route, over the runner's homes with `pitcrew_ingest::scan` (see "The machine scan") |
| `POST /v1/hooks/{engine}/{event}` | `pitcrew-api` into the runner's `RunnerHooks` (see "The runner"); with `--no-runner`, logged at debug (engine, event, member; never the body) |
| `GET /v1/sessions/{id}/terminal` | `pitcrew-api` over `SessionTerminals` (see "Terminals") |
| `POST /v1/sessions`, `POST /v1/sessions/{id}/send`, `/keys`, `/interrupt`, `/end` | `src/sessions.rs`, device routes, through the runner's `RunnerCommands` (see "Terminals") |
| `GET /v1/sessions/{id}/transcript` | `src/transcripts.rs`, a device route, from the runner's `RunnerTranscripts` (see "The runner") |

On development TCP only, the daemon answers CORS as the mock hub does: preflights from
`http://localhost:<port>`, `http://127.0.0.1:<port>` and the Tauri app's origins get `204` and
`Access-Control-Allow-*`; other origins get `403`, and so do WebSocket upgrades from them.

## Recaps

`GET /v1/recaps/blocks` and `GET /v1/recaps/days` (api-v1, "Recaps"; device tokens only) are
answered by the hub's recap index: hub-work's `RecapIndex`, which its one `WorkService`
implements (hub-work's README, "Recaps"). Blocks and day paragraphs are derived from the log, so
a restart builds them again. The blocks are kept on disk, not in memory, in `recaps.sqlite3` in the
state directory (`WorkService::with_recap_file`): a cache of this run's, replaced when the index is
built and removed when the daemon stops cleanly. On a network or unknown filesystem, the cache
moves to a private local folder (temp before `$XDG_RUNTIME_DIR`), or stays in memory if neither
base works. Unix names that folder from the canonical requested path and uid and reuses a hard
kill's leftover only when `lstat` shows this user's real 0700 directory, replacing its cache.
Otherwise, and on Windows, the name is random; hard-kill leftovers remain until temp cleanup.
See hub-work's README, "Recaps", for placement and lifecycle details.

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
  (67 revisions); a stop during a long warm-up waits for it before the store closes, within the
  stop's bounds (see "Stop").
- **Any `tz`.** The daemon computes days at any offset from −840 to 840; the mock serves only
  `tz=0` from its fixture.
- **Not the mock's fixture.** The seeded demo's log is not the fixture's slice (the seed's own
  events are activity too), so the blocks and days differ from `demo-recaps.json`; hub-work's
  README, "The seeded demo is not the fixture", says how. The parity replay compares properties.

## The machine scan

`POST /v1/machines/{id}/scan` (api-v1, "Machine scan"; device tokens only) is onboarding's scan
step: `pitcrew_ingest::scan` over this machine's agent homes, its progress and then its report
streamed back as newline-delimited `ScanFrame`s (`pitcrew_protocol::scan`). It is `src/scan.rs`,
one module and one line of the routes in `src/serve.rs`.

- **The homes are the runner's** (see "The runner"): `--homes` when given, none with `--demo`
  alone (an empty report), else this user's own. With `--no-runner` the hub reads no agent home,
  so a scan is `409`.
- **Only the hub's own machine**, the workspace's first local one (as `serve` picks it). An
  unknown or malformed id is `404`; another machine of the workspace is `409`, saying scanning it
  is not supported yet. A fresh hub has no machine until it is set up.
- **One at a time.** A scan holds its machine from the moment it is accepted until its walk ends;
  a second meanwhile is `409`. The walk cannot be stopped part-way (`pitcrew_ingest::scan` takes
  no cancel), so a client that goes away leaves it to finish; its result is dropped, and only then
  is the machine free again. A panic in the walk is an `error` frame, and frees the machine too.
- **The walk** runs on tokio's blocking pool; the scan's own threads do its reads (a 64 KiB
  prefix of each transcript, one indexed row of an OpenCode store). A first `progress` frame goes
  out at once; the walk's ticks (at most every 100 ms) wait for no client: one reading slowly
  misses some, never the last tick or the last frame.
- **What it logs:** one line per scan, `scanned this machine's agent homes`, with its counts and
  how long it took, at info. Never a path: the report goes to the person who asked, and nowhere
  else.

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

A development daemon on its own state directory gets a tmux server, or a pitcrew-ptyd, of its own,
next to a real PitCrew's (see "Terminals"). `cargo build` puts `pitcrew-ptyd` next to `pitcrewd` in
`target/debug` when it builds the workspace (`cargo build -p pitcrew-ptyd` builds it alone).

`--demo` works once per state directory; restart without it to keep the data, or use a new
directory for a fresh demo. With `--demo` the runner watches no agent home; `--homes <dir>` points
it at one (a folder with a copy of `crates/fixtures/data/transcripts/claude/demo-session.jsonl`
under `<dir>/.claude/projects/<any>/`, say), and a restart without `--demo` watches your own. The CLI takes the same token from the file:
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
after them still line up although the two servers hold different numbers of blocks.

The mock hub has no back office; with the daemon's on (the default), a check that reads the newest
event right after a write may see `@office`'s reminders instead, depending on timing.
`--no-office` runs the daemon without it, to compare the hub alone.

## Tests

**No test watches a real agent home.** Every daemon the tests start (`tests/common`) gets a home
folder of its own, `<state dir>-home` in the test's temporary folder, on every platform
(`pitcrew_fixtures::homes`): `HOME` and `USERPROFILE` are that folder and `APPDATA` and
`LOCALAPPDATA` are in it (on Windows the agent homes come from `USERPROFILE`, and so does the
default state directory), with `CLAUDE_CONFIG_DIR`, `CODEX_HOME`, `XDG_DATA_HOME`,
`XDG_CONFIG_HOME`, `OPENCODE_CONFIG_DIR`, `HOMEDRIVE` and `HOMEPATH` removed. Every start checks
that first (`check_private_home`), and `a_demo_watches_no_home_of_its_own` checks the outcome: with
`--demo` no home is watched, and without it exactly the three homes in that folder. Transcripts
come from `crates/fixtures`, under synthetic session ids.

**No test touches a real tmux or a real pitcrew-ptyd.** Every daemon the tests start gets
`--tmux-socket` (`Tmux` in `tests/common`): by default a socket in a folder whose parent does not
exist, which the runtime refuses (making nothing); and `--ptyd`: by default a path in a folder
that does not exist (`missing_ptyd`), so the daemon has no PTY runtime either (making nothing for
its endpoint), and serves without a terminal runtime, as on a machine with neither. The tmux tests
give their daemons private sockets in their temporary folders, or a `TMUX_TMPDIR` there for a
state directory's default socket (the helper refuses a daemon on its default socket without one).
The PTY tests give theirs the pitcrew-ptyd built next to `pitcrewd` and an endpoint in their
temporary folders (`--ptyd-endpoint`), or a `TMUX_TMPDIR` there for the state directory's own (the
helper refuses a real `--ptyd` without one of them on Unix; on Windows the state directory's own
pipe is the test's alone). None uses PitCrew's own per-user directory, the user's tmux server or
the user's ptyd.

`tests/serve.rs` starts the real binary on a temporary state directory and a free port, and
covers `--version`, `token show-path`, tokens and scopes, the work routes (with the workspace,
sessions, and a dispatch that answers 503 and records nothing), the stream (a move
appears on it; a reconnect with `since` gets what it missed), hooks, terminals (`404` for this
machine's session, `503` for another machine's), transcripts (an empty page for a demo session),
CORS and the `Host` guard, the single-daemon lock, and on Unix a SIGTERM stop: a clean store, the
back office stopped before it, `--demo` refused afterwards, a stream closed with 1001, and a
restart that keeps the token, the log and the data. Also on Unix: `--listen unix:<path>` binds
exactly that socket in a 0700 directory, and SIGTERM and SIGHUP both remove it (the back office
stopping first); `--version` creates nothing. Nothing it logs, at debug, holds a token. Its checks
allow for the back office appending after a write (the demo's asks are old by the wall clock).

`tests/runner.rs`, with temporary homes (`--homes`) filled from the Claude fixture:

- the transcript is a session of the hub's machine through the API (`GET /v1/sessions`), with its
  transcript (`GET /v1/sessions/{id}/transcript`, paged by one item and from `before`; bad
  queries `400`); lines appended to the file reach `GET /v1/stream` live (tool runs, the edit, the
  turn's end), the session goes idle, the transcript grows, and each record is in the log once;
- transcript pages come from the runner: an indexed transcript's pages; the demo's session of this
  machine, which the runner never indexed, an empty page at the start; and once the transcript is
  deleted, `503 unavailable` saying it is gone, with no path;
- hooks through the real route and real tokens: `@writer`'s hook changes its own session; its
  hooks on `@reviewer`'s session and on a session without an agent change nothing; the person's
  change `@reviewer`'s (an agent they own) and the agentless one. Each refused hook would leave a
  state no allowed one does (idle, or another status line), and is sent before an allowed one, so
  timing cannot pass it. Agents are given as a dispatch would, by a `session_discovered` naming
  them, appended from the test with the work model's projections;
- `--demo` without `--homes` watches no home, not even the daemon's own (it holds a transcript
  that never appears): the runner runs but `capabilities` is `[]`, and the log says so; started
  again without `--demo`, the same home is watched by default and the session appears;
- `--no-runner`: roles `["hub"]`, hooks only logged, terminal and transcript `503`, and `--homes`
  refused with it;
- a runner that cannot start (`runner` in the state directory is a file): the daemon starts, warns
  with the folder, and serves as a hub only (roles `["hub"]`, terminal and transcript `503`, the
  work routes as ever);
- on Unix, SIGTERM: `the runner stopped` and `the back office stopped` before `store closed`; a
  restart with lines written meanwhile keeps the session's id, adds the rest, and stores nothing
  twice;
- on Unix, a stop with a read that does not return for minutes (the transcript grown by a
  terabyte with no line break, a sparse file that takes no disk blocks, which both the watcher and
  a transcript request then read to the end of its line): the daemon still exits cleanly, within
  30 seconds, logging the runner, the work on the blocking pool and the store it left behind;
- on Unix, a transcript swapped for a named pipe is refused as soon as it is opened (`is a named
  pipe, not a regular file`), by the watcher and by a transcript request, which is answered within
  5 seconds; a stop then takes under 8 seconds and leaves nothing behind.

`tests/terminals.rs`, the runner's terminals:

- without tmux or pitcrew-ptyd (the refused socket and the missing ptyd every test daemon has by
  default): host info has neither `tmux` nor `pty`, the log warns why for both (naming where
  pitcrew-ptyd was looked for, and warning of `--tmux-socket` and `--ptyd`), the demo's session
  of this machine has no terminal (`404`), `POST /v1/sessions` is `503`, a command for a session
  without a terminal `409`,
  for another machine's `503`, for an unknown one `404`; malformed bodies `400`; each bound one
  past its limit (`text`, `keys`, `brief`, `cwd`, a body past 1 MiB) `400 invalid`, and at its
  limit passes; a relative or missing `cwd`, and one under a folder every user can write to,
  `400`, while one under a group-writable folder goes on to the missing runtime (`503`) and the
  log names that folder at info; a
  session of another person's agent (added through the hub's events) `403` for `send`, `keys`,
  `interrupt` and `end`, before its lack of a terminal, while one of the person's own agent gets
  to that `409`; an agent token `403` on all five routes;
- in tmux (Unix, tmux 3.2 or newer; skipped with a message otherwise, or failed when
  `PITCREW_REQUIRE_TMUX=1`), with a private socket, a stand-in `claude` first on the daemon's
  `PATH` (it writes its transcript, in its `CLAUDE_CONFIG_DIR`, for the `--session-id` it is given,
  then prints each byte it reads as `KEY <hex>`), and `--homes`: host info has `["tmux",
  "watch"]` and the log the attach command; `POST /v1/sessions` answers `202` with the session,
  `terminal` set, and its window is in tmux (`claude work`); a start for the demo's cluster (`503`),
  with `bypass_permissions` or a `--dangerously-…` model (`400`) starts nothing; the terminal
  WebSocket streams the stand-in's output; `send`, `keys` and `interrupt` reach it (`KEY 68`, `KEY
  69`, `KEY 09`, `KEY 1b`). SIGTERM: `the runner stopped`, then `the terminals' runtime detached`,
  then `store closed`; the pane is alive, and its `@pitcrew-offset` is exactly where the stream
  ended. The next daemon reports `tmux` again and finds the terminal: a stream from that offset
  gets the new output with nothing lost, one from `0` a `truncated` frame naming the offset. A
  second session, its folder given as `<work>/../work`, runs in the resolved folder and is named
  after it; ended `graceful`, it prints its goodbye, its stream ends with `exit`, and it is
  `ended`. A third, in a folder under a group-writable one (mode 2775), starts (`202`), the log
  names that folder at info, and `kill` ends it. `kill` ends the first (`exit`, close `1000`,
  `ended`), and a command for it is then `409`. With all ended, tmux's server exits, and once the
  daemon has stopped nothing with the test's mark (`PITCREW_TEST_RUN`, which the daemon's tmux
  server and panes inherit) is left;
- two daemons on two state directories, on their default sockets (under the test's
  `TMUX_TMPDIR`): two sockets, in directories named by 8 hex digits, two servers, each holding only
  its own session's terminal; a third daemon given the first's socket finds it locked
  (`another pitcrewd uses this tmux socket`), has no `tmux`, and touches nothing;
- a tmux server with a session of someone else's (`mine`): the daemon has no `tmux`, says the
  server has sessions `that are not PitCrew's`, and leaves it as it was, before and after its
  stop;
- whatever the outcome, each test kills its servers (`tmux -S <its socket> kill-server`) and
  anything still marked.

`tests/pty.rs`, the runner's terminals in pitcrew-ptyd, forced (`--terminal-runtime pty`) even
where tmux is installed, on Linux, macOS and Windows: the pitcrew-ptyd cargo built next to
`pitcrewd` (`--ptyd`; the tests that need it say they are skipped when it is not built, and fail
instead under `CI` or with `PITCREW_REQUIRE_PTYD=1`), an idle exit of half a second
(`--ptyd-idle-exit-ms`), `--homes`, and a stand-in `claude` first on the daemon's `PATH` (as in
the tmux tests; on Windows a `claude.cmd` running a PowerShell script, as npm's shims do, that reads
keys with Ctrl-C as input). ptyd is looked at through a `PtyRuntime` of the test's own on the same
endpoint, which lists and reads and never starts one:

- the whole life of a session: host info has `["pty", "watch"]`, the log says the terminals run in
  pitcrew-ptyd on the test's endpoint and warns of each override, and no ptyd runs before the
  first terminal; `POST /v1/sessions` answers `202` with the session, `terminal` set, and ptyd
  holds exactly that terminal (`claude work`, no `native_target`); a start for the demo's cluster
  (`503`), with `bypass_permissions` or a `--dangerously-…` model (`400`) starts nothing; the
  terminal WebSocket streams the stand-in's output; `send`, `keys` and `interrupt` reach it (`KEY
  68`, `KEY 69`, Enter, `KEY 09`, `KEY 1b`), and on Unix ptyd holds the same stream byte for byte.
  A `d` makes the stand-in print `DELAYED` two seconds later, while the daemon stops: on Unix the
  log has `the runner stopped`, then `the terminals' runtime detached` (naming pitcrew-ptyd), then
  `store closed`; the same ptyd keeps the terminal alive and holds `DELAYED`. The next daemon
  reports `pty` and finds the terminal: a stream from the old offset gets `KEY 64` and `DELAYED`
  with nothing lost (no `truncated`), and one from `0` the whole output, also without
  `truncated`. A second session (its folder given with `..` on Unix) ended `graceful` prints its
  goodbye, its stream ends with `exit`, and it is `ended`; `kill` ends the first (`exit`, close
  `1000`, `ended`), and a command for it is then `409`. ptyd still lists both, ended. With the
  daemon stopped, ptyd exits once idle (on Unix removing its socket), and nothing of the test's is
  left: nothing with its mark (`/proc` on Linux, `ps -E` on macOS), or on Windows no process whose
  command line names its folder or its pipe;
- two daemons on two state directories, on their own endpoints (under the test's `TMUX_TMPDIR` on
  Unix, the user's pipe with the state directory's digits on Windows): each endpoint is the one the
  daemon's rule gives, in the log; each daemon's session is the only terminal of its own ptyd, and
  the two ptyds are two processes; on Unix a third daemon given the first's endpoint finds it
  locked (`another pitcrewd uses this pitcrew-ptyd endpoint`), has no `pty`, and the first's ptyd
  is not touched;
- a missing pitcrew-ptyd (forced, where the daemon is told to look): no runtime, a warning naming
  where it looked (`pitcrew-ptyd is not installed at …`) and that tmux was not tried, `POST
  /v1/sessions` `503`, and nothing made for the endpoint.

`tests/office.rs`, with `--demo`, appends a dispatch's end straight to the store, as another
writer would (the daemon looks at it with its next append, here a comment through the API, or at
its next start):

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

`tests/dispatch.rs` (Unix), dispatch end to end: `--demo`, temporary homes (`--homes`), the
runner's terminals in the pitcrew-ptyd built next to `pitcrewd` (`--terminal-runtime pty`, an
endpoint of the test's; skipped with a message when it is not built, unless `CI` or
`PITCREW_REQUIRE_PTYD=1`), and stand-in `claude` and `codex` scripts first on its `PATH`, which
write their transcript as the CLI does when given a prompt and note what they were given. Every
process the daemon starts carries the test's mark, and none is left at the end.

- for Claude (`@writer`) and Codex (`@runner`), each in a project folder of the test's: the
  dispatch's session appears once, under the dispatch's id, linked to the task, with its terminal;
  the CLI got `PITCREW_TOKEN_FILE` naming `agents/<agent>.token` (0600), an agent token for that
  agent and `@sam` (`GET /v1/me` is the agent; it may not dispatch; it is not the device token),
  and `PITCREW_URL`, and no token in its environment, though the daemon was started with the
  person's token in `PITCREW_TOKEN` and another socket in `PITCREW_SOCKET` (read as the CLI reads
  them, `pitcrew_cli::config`, its variables give the agent's token and the daemon's address);
  the agent's own hooks change the session;
  its first `working` moves the task to in progress; the agent's move to review (its report)
  finishes the dispatch as succeeded, authored by the agent for `@sam`; ending the session leaves
  it so;
- after a crash (`kill -KILL`) with a dispatch whose CLI runs (its transcript held back) and one
  appended as if the hub stopped before starting it: at the next start the second fails ("did not
  start") and its session ends, while the first is kept and reported under its id once its
  transcript appears; ended without a report, its dispatch is `canceled`.

`tests/board.rs` (Unix), a board draft end to end, with the same rig as `tests/dispatch.rs` and
the `pitcrew` CLI built next to `pitcrewd` (skipped with a message when it or pitcrew-ptyd is not
built, unless `CI` or `PITCREW_REQUIRE_PTYD=1`): a stand-in `claude` answers the draft as the
prompt asks, through the real `pitcrew board submit` with the token file the daemon gave it; its
proposal arrives and a second is refused (exit 4); the prompt is the one the preview measured, and
names the draft and the workstream's session; no token is in its environment; no task exists
until the person reviews it, and the review creates the accepted task only, labelled `drafted`,
linking its evidence session.

`tests/setup.rs`, the first run, fresh daemons with temporary homes (`--homes`, holding the
Claude fixture's transcript):

- a fresh start: `setup_needed` is true and the name "Workspace", `GET /v1/me` is `404`, host
  info has roles `["hub"]` and no capability, no `@office`, the office and the runner logged off,
  no session though a transcript waits in the home, no `workspace.json`; a setup asking for
  `@office` is `409` and changes nothing;
- `POST /v1/setup` with padded names (stored trimmed), then without a restart: `GET /v1/workspace`
  has the name and no `setup_needed`, `GET /v1/me` is the person, `workspace.json` holds the id
  and the name, `@office` is an agent owned by the person and acts (a dispatch of an in-progress
  task finishing moves it to review, authored by `@office` on behalf of the person, as in
  `tests/office.rs`), host info shows the runner and `watch` (`HEAD` too: no body, and the
  `Content-Length` of `GET`'s answer, before setup and after), and the transcript is a session of
  the new machine. A second setup is `409`; no token is in the logs. A restart keeps the name,
  shows the runner at once, starts the office from its start, finds the same `@office` (one), and
  keeps the session;
- two setups racing: exactly one `200`, one `409`, one person, one `@office`, one set-up log line;
- `pitcrewd init`: with no daemon ever started there, exit 5 saying to start `pitcrewd serve`, and
  nothing created; against the running daemon (its private socket on Unix, development TCP
  elsewhere, never the user's own pipe) the line it prints, a bare handle given its `@`, and no
  token printed; again, the API's `409` (exit 4); a malformed handle, the API's `400` (exit 2);
  its daemon stopped, exit 5 again;
- `pitcrewd connect` without a socket, or with an unknown argument: exit 2, nothing on stdout;
- on Unix, `pitcrewd connect --socket <dir>/pitcrewd.sock` to a daemon on `--listen unix:` carries
  `GET /v1/host/info` (after the ready mark with the nonce) and an authenticated
  `GET /v1/workspace` through stdin and stdout, creates nothing in its home, and with no daemon
  exits 4.

`tests/scan.rs`, the machine scan, over temporary homes laid out from the Claude, Codex and
OpenCode fixtures (their folders moved into the test's own, two of them git repositories):

- the answer is `application/x-ndjson`: `progress` with `scanned: 0` first, ticks that only grow
  to `scanned == total`, then `done`; its report counts the three sessions by engine, home, folder
  and month (the earliest start is the Codex fixture's), and suggests the paper's repository with
  its `paper` folder as a workstream, the runs folder (no `.git`) and the tools' repository; the
  log has the counts and not the paths;
- a second scan while the first is held (`--scan-hold-ms`) is `409`, and the first still ends
  with its report; a client that goes away keeps the machine taken until its walk has ended, and
  then it scans again;
- no token `401`, an agent `403`, an unknown or malformed machine `404`, the demo's cluster `409`
  ("not supported yet"); homes that hold nothing give an empty report with every list present;
- `--no-runner`: `409`, naming it;
- a fresh hub: `404` before setup; after it, the new machine is scanned in the daemon's own
  (test) home folder, as the runner would watch it.

The unit tests in `src/scan.rs` check that a machine has one scan at a time (another machine its
own), that its place comes back when the walk panics, which machine is the hub's own, that a frame
is one line of JSON, and that a walk over no home ends with its last tick and an empty report, also
for a client that has gone. `src/cli.rs` checks that `--scan-hold-ms` parses and is hidden.

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
that has a person (and not before), through the work model's `ensure_office_member`; keeping the
office off when `@office` is a person, another person's agent or no one's agent; `--no-office`
removing `office.json`; the start point saved before the loop runs; the loop over the demo across
a restart; a save that cannot write, tried again until it can and at stop; which of the hub's failures are tried again; and, on a store
forced into network mode, its single-host lease taken once (the lease's clock is read once, at
acquisition) and held, the same file, while the office starts and acts, then let go of when the
store closes. (A lease let go of and taken again on a free path gets the same generation number,
so the number alone could not show it.)

The unit tests of the runner's wiring: `src/agents.rs` answers each session's agent and owner,
sees a session gain an agent at once (and keep it through a re-statement without one), resolves
sub-agents up their chain (16 sessions resolve; 17, or 16 below a parent not stored, are
`Unknown`), and answers `Unknown` for a chain that disagrees or loops and for an agent the hub
does not know as one; `src/terminals.rs` answers `404`, `503` or the runner's answer by where the
session is, with and without a runner, and with one attached later; `src/host.rs` answers the
runner's role, `tmux` or `pty` and `watch` (each, both, neither) once one is attached, and `HEAD`
with `GET`'s headers and no body; `src/transcripts.rs` answers `404`, `503`, or an empty page for a
session of this machine the runner never indexed, with and without a runner; `src/sessions.rs`
checks the bodies and every bound at its limit and one past it, the hook rule for who may command
a session (each scope against no agent, an owned agent, another's, one without an owner, and an
unknown one), which sessions take a command (`404`, `503`, `403`, `409`), what a start needs (a
person, an absolute folder, a machine of the workspace, no `agent` or `task`), folders resolved
(`..` and links), refused (relative, missing, a file, under a folder every user can write to
unless sticky, writable by every user themselves) and allowed when group-writable, themselves or
above, with those folders named; and a start with no runtime `503`; `src/runtime.rs` checks that
a let-go runtime answers `Unavailable` to every call and is dropped, that a refused socket means
no tmux and makes nothing, that each state directory has a socket of its own (the same however spelled), that
socket directories are made private (and an open one refused, not repaired) and locked once, and
which sessions of a server are not PitCrew's (a stand-in `tmux` answering); and for the PTY
runtime, that ptyd's endpoint is next to the tmux socket on Unix, under the same lock (held by one
runtime at a time, whichever takes it), and the user's pipe with the state directory's digits on
Windows; that pitcrew-ptyd is looked for next to the running executable by default; that a refused
socket and no ptyd mean no runtime and make nothing for either; that without tmux, or forced, a
program file where ptyd is looked for gives the PTY runtime (nothing started), whose endpoint a
second daemon then finds locked (Unix); and that reasons read plainly; `src/runner.rs` and
`src/cli.rs` check that `--demo` alone watches nothing (the person's homes are not even looked
up), how `--homes` values become homes, and that `--no-runner` refuses `--homes`. `src/cli.rs`
also checks that everything after `connect` reaches the bridge as it is, `init`'s arguments, and
that `--tmux-socket`, `--ptyd`, `--ptyd-endpoint`, `--ptyd-idle-exit-ms` and `--terminal-runtime`
parse (a bad value refused) and are hidden from help;
`src/init.rs` the request's body (a bare handle given its `@`) and the client pointed at the state
directory's socket, or the `--listen` given; `src/setup.rs` that the listener hands the setup over
once, and that an office loop and a runner (real, watching nothing) are kept until the stop begins
and handed back, not kept, once it has.

## Not wired yet

- Agent tokens for sessions PitCrew did not start: a CLI a person runs by hand has no agent and
  no token of PitCrew's, and how such a session would authenticate as an agent is a separate
  design. Agent token files are not revoked when an agent's sessions end (a token is reused for
  its next).
- The queue, per-agent concurrency and hand-off (hub-work E.md item 4), and dispatch to another
  machine's runner (the JSON-lines link): such a dispatch answers `503`.
- Bundling `pitcrew-ptyd` next to `pitcrewd` in the installers (stream P, `P-desktop-bundle`):
  until then, an installed `pitcrewd` without tmux (Windows above all) has no terminal runtime.
- On Windows, no lock guards ptyd's pipe beyond the state directory's own: a daemon given another
  daemon's pipe with the hidden `--ptyd-endpoint` would share that ptyd.
- The first prompt (`brief`) is passed to the CLI as an argument, so other users of the machine
  can read it in the process list (`/proc/<pid>/cmdline`), and tmux shows it in the pane's
  `pane_start_command`. Passing it on the CLI's standard input, or through a private file, would
  keep it to the user (threat model O43).
- Folders on a Windows drive mounted in WSL without metadata (mode 777) are refused as a session's
  `cwd` (see "Terminals"), by decision: there is no exception for `/mnt/<drive>`.
- Host info from `pitcrew-api` itself: its `router` takes a fixed `HostInfo`, so the daemon answers
  `GET /v1/host/info` in a layer of its own (see "The runner"). Proposal for stream H: `router`
  takes a source of host info (`Arc<dyn Fn() -> HostInfo>`, or a `watch::Receiver`), and the
  layer goes.
- `--demo` through the setup path (the demo still seeds its own person, machine and name).
- Remote machines (the desktop's side of the tunnel, and a supervisor of the local daemon), the
  Tauri shell, auto-start and installers.
- Scanning another machine of the workspace (`POST /v1/machines/{id}/scan` is `409` for one), and
  stopping a scan part-way: `pitcrew_ingest::scan` takes no cancel, so a scan whose client went
  away runs to its end. A cancel flag in its `ScanOptions` (stream A) would let the route stop it.

## Linking sessions

`HubLocations` reads workstream locations and current session link bases from the hub. The runner
links by the deepest folder, then matching branch at equal depth; ties link neither. A store
subscription compares location snapshots after appends (and on lag) and asks the runner to relink
existing sessions when locations change. It starts before serving, includes runners started after
setup, and is canceled when serving ends.

`POST /v1/sessions/{id}/link` is person-only. A workstream and optional task, or a task alone,
make a manual link. Imported links, like manual/dispatch/claimed links, are firm and are never
overwritten by folder/branch inference. `tests/linking.rs` covers synthetic transcript discovery,
branch preference, later creation, and manual-link protection.

## Workstream files

The device-only Files API resolves location roots from the work model and calls the runner on
the blocking pool. Remote and WSL locations answer 501. Writes require a revision or explicit
null for creation; JSON bodies are capped at 12 MiB and decoded files at 8 MiB. State now includes
file-backups: private bounded originals before replacement (runner README, Workstream files).
Responses use no-store and nosniff, and failures log counts and fixed reasons without paths.

`import.json` holds the durable session inclusion choice, scoped to this state directory. `GET /v1/import`, `POST /v1/import/dry-run`, and `PUT /v1/import` are device-only. The runner keeps reading in place; the API visibility adapter hides excluded session events, and transcript/terminal reads return 404 for them.

## Onboarding hooks and safety

Device-only `POST /v1/machines/{id}/hooks/diff` previews hooks using the CLI installer for supported CLIs on PATH. `hooks/install` confirms that exact revision with stale-file checks and existing backups. Previews are person/machine-bound, expire after ten minutes, and are lost on restart. Config contents are never logged. Only this hub’s own machine is supported; other machines return 501. The installed `pitcrew` executable must be beside `pitcrewd` or on its PATH. `GET`/`PUT /v1/safety` persist workspace preferences; new sessions use the saved permission mode unless explicitly overridden.

Onboarding review: hook previews detect supported CLIs on PATH or through their
homes, skip conflicting engines while applying other changes, and report the
skipped engines. No-change previews cannot set the wizard's installed flag.
Desktop packages include the hook CLI beside the daemon. Safety uses snake_case
wire fields and the shared PermissionMode enum; bypass defaults are currently
refused. Unsaved safety reports `saved: false` for legacy per-task acceptance.
