# @pitcrew/mock-hub

A fixture server that speaks PitCrew's API v1 ([`docs/build/contracts/api-v1.md`](../../docs/build/contracts/api-v1.md)),
so the UI can be built without the Rust daemon. It serves the demo workspace in
[`crates/fixtures/data/demo-workspace.json`](../../crates/fixtures/data/demo-workspace.json) from
memory, and every change you make lasts until the server stops. Its recaps come from
[`demo-recaps.json`](../../crates/fixtures/data/demo-recaps.json) beside it.

No dependencies: only Node's built-in modules. Node 22.18 or newer runs the TypeScript directly.

## Run it

```sh
node apps/mock-hub/src/server.ts          # from the repo root
npm run mock-hub                          # the same, via the root package.json
PORT=4400 node apps/mock-hub/src/server.ts
```

PowerShell: `$env:PORT = '4400'; node apps/mock-hub/src/server.ts`. `PITCREW_MOCK_SCAN_WINDOW`
sets how many revisions a filtered `GET /v1/events` examines (default 500).

It listens on `http://127.0.0.1:47317` (never on other interfaces) and prints one line per request.

| Token | Member | Scope |
|---|---|---|
| `dev-device-token` | `@sam` (`01JB000000000000000MEM0001`), the person | `device`: every route |
| `dev-agent-token` | `@writer` (`01JB000000000000000MEM0002`), an agent owned by @sam | `agent`: routes marked **agent**; reads everything, writes only to its own tasks and sessions |

### Fresh mode: starting before setup

`PITCREW_MOCK_FRESH=1 node apps/mock-hub/src/server.ts` (PowerShell: `$env:PITCREW_MOCK_FRESH = '1'`)
serves an empty workspace instead of the demo: no members, machines or work (projects, tasks,
sessions, asks, events), and `GET /v1/workspace` answers `setup_needed: true`. The two tokens still
authenticate, but neither has a `Member` yet, so `GET /v1/me` answers `404` for both until
`POST /v1/setup` is called with the device token (api-v1.md, "The first run"):

```sh
curl -X POST -H "Authorization: Bearer dev-device-token" -H "Content-Type: application/json" \
  -d '{"workspace_name":"Demo Lab","person":{"name":"Sam Rivera","handle":"@sam"},"machine_name":"This laptop"}' \
  http://127.0.0.1:47317/v1/setup
```

Setup appends `member_added` and `machine_added` (one call to `hub.append` each, delivered to a
connected `GET /v1/stream` client in the same batch), makes the device token's member `@sam`
(`01JB000000000000000MEM0001`, the same id the demo uses), and from then on the mock behaves like
the demo, minus its seeded members, machines and work. `startServer({ fresh: true })` sets this up
for tests (`ServerOptions.fresh`); `test/setup.test.ts` covers both modes.

```sh
curl http://127.0.0.1:47317/v1/host/info
curl -H "Authorization: Bearer dev-device-token" "http://127.0.0.1:47317/v1/tasks?status=todo"
curl -X POST -H "Authorization: Bearer dev-agent-token" -H "Content-Type: application/json" \
  -d '{"to":"in_progress"}' http://127.0.0.1:47317/v1/tasks/PAP-2/move
```

```powershell
Invoke-RestMethod http://127.0.0.1:47317/v1/host/info
Invoke-RestMethod "http://127.0.0.1:47317/v1/tasks?status=todo" -Headers @{ Authorization = 'Bearer dev-device-token' }
```

From a browser, WebSockets take the token as a subprotocol:

```js
const stream = new WebSocket('ws://127.0.0.1:47317/v1/stream?since=15', [
  'pitcrew.v1',
  'pitcrew.bearer.dev-device-token',
]);
```

## What it does

- **Read cursors.** Device-only `GET /v1/me/cursors` and `PUT /v1/me/cursors/{scope}`,
  forward-only for each person and scope, with `cursor_moved` on a forward write.
  `dev-second-device-token` represents a second synthetic person for isolation tests.
  Cursor events appear only in the author's device streams, including replay; activity
  and recaps exclude them. Activity pages count real events and return their actual revisions.
  Like other mock state, cursors last until the server stops.

- **Every route in the contract**, with the `ApiError` body and status for each failure (400, 401,
  403, 404, 409, 503). Task routes accept an id, a prefixed id (`tsk_…`) or a key (`PAP-4`).
- **The first run** (`POST /v1/setup`, "Fresh mode" above). `GET /v1/workspace` answers
  `setup_needed: true` while the workspace has no person. In demo mode setup always answers 409; in
  fresh mode it validates `workspace_name`, `person.name`, `person.handle` and `machine_name`
  exactly as the contract says (the three names trimmed with `String.prototype.trim`, then
  counted in code points and stored trimmed; the handle's shape, not trimmed; no control
  characters), answers 409 once already set up, on a handle clash, or for `@office` (reserved for
  the back office, though the mock has none), and otherwise appends `member_added` and
  `machine_added` for the device token's own member, so it and `GET /v1/me` mean that person from
  then on.
- **Auth and scopes.** Agent tokens reach only routes marked **agent**. They read the whole
  workspace, but write only to their own tasks (assignee, or holder of an active dispatch) and
  sessions; any other write, a move included, is `403 forbidden`. An agent's subtask list replaces
  only its own plan lines. `author` and `on_behalf_of` always come from the token.
- **Asks.** A device token answers asks to its person or to agents that person owns; an agent
  answers only questions and mentions addressed to itself.
- **Move rules** are `TaskStatus::can_move` ported exactly; a refused move is `409 conflict`.
- **Dispatch** assigns an unassigned task to the agent (`task_assigned`), then emits
  `dispatch_started` and `session_discovered`. Done or canceled tasks answer 409.
- **Creating projects and workstreams** (`POST /v1/projects`, `POST /v1/workstreams`) with the
  contract's defaults; a project key already in use is 409, an unknown project for a workstream 404.
- **The machine scan** (`POST /v1/machines/{id}/scan`): a fixed synthetic report (14 sessions in
  three agent homes under `/home/sam`, the fixtures' folders and a few more, and three suggested
  projects with workstreams from sub-folders and branches), as newline-delimited frames: a
  `progress` frame at once, four more over about a second (`startServer({ delays: { scan } })`),
  then `done`. Only the hub's own machine (its first `local` one); another is 409, an unknown one
  404. One scan at a time: a second is 409 until the first has written its report, even if its
  client went away. It reads no folder.
- **Machine setup** (`GET /v1/machines/{id}/check`, `…/agents`, `…/agents/{engine}/sign-in`
  with `GET`, `POST` and `DELETE`;
  `src/machine-setup.ts`): a fixed check of a synthetic laptop (Claude Code, Codex, tmux and git
  there; OpenCode and gh missing, with `install_page`; no SLURM), accounts (Codex signed in with
  ChatGPT, Claude Code not, OpenCode not installed), and a sign-in whose terminal
  (`/v1/sessions/{terminal}/terminal`) shows a canned login, echoes keys, and ends on Enter or by
  itself after about two seconds (`startServer({ delays: { signIn } })`); Claude Code then reports
  `sam@example.com`. `DELETE …/sign-in` ends one at once (its terminal gets `exit`) and forgets
  it. The daemon's rules: the hub's owner only (its first person, `dev-device-token`'s; another
  person, `dev-second-device-token`, gets 403 on every route and on a sign-in's terminal), the
  hub's own machine only, one sign-in per CLI while it runs, 409 for OpenCode (not installed),
  `device_code` for Codex only. It runs nothing.
- **Editing tasks** (`PATCH /v1/tasks/{id-or-key}`): every rule in the contract (title, labels,
  workstream, `blocked_by` with cycles as 409, dates). `task_updated` carries only the fields that
  changed, and a patch that changes nothing emits nothing.
- **Briefs.** `GET /v1/briefs` shows each brief's pending proposal in `proposal` (the fixture's
  revision 15 is one, for PAP). `PUT` stores `next`, and `brief_accepted` carries it. Accepting the
  pending proposal unchanged copies its receipts, and the brief stays the back office's; accepting
  it or keeping the current text clears `proposal`. No route proposes a brief: tests append
  `brief_proposed` through `startServer()`'s `hub`.
- **The event log.** The fixture's 15 events are revisions 1–15; every change appends an event with
  a new ULID. `GET /v1/events` pages it (`before` is exclusive, `at_start` says whether older
  matching events exist) and filters by project, workstream, task or session. A filtered request
  examines at most 500 revisions (`PITCREW_MOCK_SCAN_WINDOW`, or `startServer({ scanWindow })`), so
  across a long gap it answers empty pages that are not at the start, as the contract allows.
- **Recaps.** `GET /v1/recaps/blocks` and `GET /v1/recaps/days` serve the recap engine's output
  for the demo's 15 events (`crates/fixtures/data/demo-recaps.json`: 10 blocks with their lines,
  and each project's day paragraphs at UTC), with the contract's filters, paging and errors.
  `cargo test -p pitcrew-fixtures --test recaps` fails when that file no longer matches the engine.
- **`GET /v1/stream`**: `hello`, then the missed events when `since` is behind, then live events
  batched over 60 ms, and a `ping` every 20 s.
- **Simulated sessions.** A dispatch or a new session starts in `starting`; about 1.5 s later it
  turns `working`, and a dispatched agent moves its task to in progress. `send` records the prompt,
  then a canned reply and `turn_ended` follow about 0.8 s later. Escape, Ctrl-C and `interrupt`
  stop the turn; `end` emits `session_ended`. Answering an ask resumes its waiting session.
- **Transcripts** for all six demo sessions, paged tail-first with `before` and `limit`. SES0001
  is the Claude fixture transcript (its offsets are the file's real record offsets), and the
  receipts in the demo workspace point at real items.
- **Integrations** (`/v1/integrations…`, `src/integrations.ts`): GitHub and Jira connections over
  the recorded exchanges in `fixtures/` (`github.fixture` for `example-org/demo-repo`,
  `jira.fixture` for project `DEMO` on `https://jira.example.com`; the sync crates' format, which
  `pitcrewd serve --integration-fixtures` reads too), or the folder `startServer`'s
  `integrationFixtures` names, read again at each sync. The checks are the daemon's. Adding one
  finds or adds the caller's own sync member (`@sync`, or `@tracker-sync` when `@sync` is another
  person's) and syncs at once, and so do `POST …/sync` and storing a credential. Each sync diffs
  what it reads against the last read, as the daemon's crates do, so only upstream changes act:
  open issues in a linked scope become tasks (`PATCH /v1/workstreams/{id}` with `external` links a
  workstream and emits `workstream_linked`), and so does an open issue moved into a linked
  milestone or epic; upstream-owned fields are overwritten when upstream changes them; an upstream
  close or reopen moves the task by the sync's `can_move`, with conflicts as asks; a milestone or
  epic seen closing ships its workstreams; and a merged pull request is noted on its task.
  Credentials stay in memory and are never returned. `gh_cli` connections always have a
  credential here.
- **Outward writes** (`/v1/writes…`, `src/writes.ts`), by the daemon's rules: after every request
  that may change something, a pass proposes what a person's change implies upstream (an
  `approval` ask from the integration's own sync member, with `write_proposed`; a title or
  description only when upstream's is held exactly, labels as a change), records a denial as not
  sent, and "sends" an approved write, or a failed one a person asked to retry
  (`write_retry_requested`), once: it starts only from the integration's own member's approval
  answered "Send" by a person; an edit, close or reopen is first checked against the issue's
  fixture as upstream has it now (a change since sends nothing), and a retried create or comment
  first looks for its earlier attempt; links are kept only on the integration's web origin. The
  fixtures' answer to each method and URL decides it (2xx sent, else failed with that status; no
  exchange at all is a failure too; a `since=` parameter is ignored, as the daemon's fixture
  transport does). A sent write changes the mock's copy of upstream, so the next sync agrees; a
  created issue becomes the task's source. Every request "sent", reads included, is kept
  (`sentRequests`) for the tests.
- **Terminals.** `GET /v1/sessions/{id}/terminal` replays a short ANSI screen, echoes keystrokes,
  accepts `{"type":"resize"}` and ignores unknown control types (malformed JSON closes with 1007),
  sends `{"type":"truncated"}` before the replay for sessions that ran over a day, and sends
  `{"type":"exit"}` when the session ends. Sessions on the unreachable GPU box answer 503.

## What it does not do

- It runs no agents and reads no real transcripts; replies, terminal screens, the scan's report and
  machine setup's check, accounts and logins are canned.
- Nothing is saved: restart the server to get the demo workspace back.
- No back office, and no tracker sync on a timer (only when asked): briefs are only proposed by the fixture, dispatches never finish
  on their own, workstream health never changes by itself, and mentions do not create asks.
- A resize changes nothing, and `model`, `persona` and `permission_mode` on a new session are only
  checked, not used (the contract says they are not echoed on `Session`).
- It computes no recaps: they stay the fixture's whatever you change, and days exist for `tz=0`
  only. Any other `tz` is `400 invalid`, although the contract allows −840 to 840.
- Terminal sockets send no WebSocket Pings, so they never close an idle client with 1013.

## Safety

It binds 127.0.0.1 only, and answers only requests addressed to `localhost`, `127.0.0.1` or
`*.localhost` (so a web page cannot reach it through DNS rebinding). CORS allows
`http://localhost:*`, `http://127.0.0.1:*` and the Tauri app (`tauri://localhost`,
`http://tauri.localhost`, `https://tauri.localhost`). Bodies are limited to 1 MiB and
WebSocket frames and messages to 1 MiB. It serves no files. The tokens are public on purpose; the
data is made up.

## Tests

```sh
node --test "apps/mock-hub/test/*.test.ts"   # from the repo root, one process per file
node --test apps/mock-hub/test/              # also works, all files in one process
```

Tests start their own server on port 0 and use a small WebSocket client of their own
(`test/ws-client.ts`). `test/index.js` exists because Node 21+ runs a folder given to `--test` as
a single entry point.

## Files

| File | What |
|---|---|
| `src/server.ts` | Entry point: HTTP, CORS, bodies, WebSocket upgrades. Exports `startServer({ port, fresh })`. |
| `src/routes.ts` | The routes, tokens and scope rules, including `POST /v1/setup`. |
| `src/live.ts` | The event stream and terminal sockets. |
| `src/state.ts` | In-memory state, the event log and its revisions; `freshWorkspace()` for fresh mode. |
| `src/simulate.ts` | Simulated session liveness. |
| `src/transcripts.ts` | Canned transcripts and paging. |
| `src/recaps.ts` | The recap routes, paged from the recaps fixture. |
| `src/scan.ts` | The machine scan: its synthetic report and streamed frames. |
| `src/integrations.ts` | GitHub and Jira integrations over `fixtures/*.fixture`, and the links' checks. |
| `src/writes.ts` | Outward writes: proposals, approvals and the recorded answers. |
| `fixtures/` | Recorded, synthetic GitHub and Jira answers (reads, and the writes' answers), shared with the daemon's tests and the conformance runner. |
| `src/machine-setup.ts` | Machine setup: the synthetic check, the accounts, and sign-in terminals. |
| `src/ws.ts` | A minimal WebSocket server (RFC 6455). |
| `src/types.ts` | Wire types mirroring `crates/protocol`. |
| `src/rules.ts` | `can_move` and date checks ported from `model.rs`. |
| `src/validate.ts` | `ApiError` failures and request validation. |
| `src/ulid.ts` | Time-ordered ULIDs. |

`tsconfig.json` is for editors only (it needs TypeScript 5.8+ and `@types/node`, which this package
does not install); nothing is compiled.

## Files

Device-only workstream file routes use a per-location in-memory tree, initially containing
src/hello.txt, a large.bin above the read cap, and an outside link that cannot be followed.
They enforce revisions, exclusive creation, path rules and body/file/list caps; writes persist
only in memory. Remote and WSL locations return 501. No filesystem paths are opened.

Session import implements all/filtered/start-fresh rules in memory on the three `/v1/import` routes. Its counts leave sub-agents (sessions with a `parent`) out and report them apart (`subagents`), and a sub-agent is included exactly when its parent is (`import.ts`, `deciding`). The synthetic scan report suggests each project's default (`main`) workstream first, with each suggestion's `kind`. Session lists, activity and stream delivery share inclusion; affected recap day paragraphs are reconstructed from retained fixture block lines. The mock still computes no new recap blocks for mutations.

Onboarding hooks use synthetic in-memory configuration text, person/machine-bound previews, stale refusal and idempotent confirmation. The mock never opens agent homes. Device-only safety read/save validates the same settings as the daemon and appends `safety_changed` on change.
