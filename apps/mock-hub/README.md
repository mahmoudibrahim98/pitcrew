# @pitcrew/mock-hub

A fixture server that speaks PitCrew's API v1 ([`docs/build/contracts/api-v1.md`](../../docs/build/contracts/api-v1.md)),
so the UI can be built without the Rust daemon. It serves the demo workspace in
[`crates/fixtures/data/demo-workspace.json`](../../crates/fixtures/data/demo-workspace.json) from
memory, and every change you make lasts until the server stops.

No dependencies: only Node's built-in modules. Node 22.18 or newer runs the TypeScript directly.

## Run it

```sh
node apps/mock-hub/src/server.ts          # from the repo root
npm run mock-hub                          # the same, via the root package.json
PORT=4400 node apps/mock-hub/src/server.ts
```

PowerShell: `$env:PORT = '4400'; node apps/mock-hub/src/server.ts`.

It listens on `http://127.0.0.1:47317` (never on other interfaces) and prints one line per request.

| Token | Member | Scope |
|---|---|---|
| `dev-device-token` | `@sam` (`01JB000000000000000MEM0001`), the person | `device`: every route |
| `dev-agent-token` | `@writer` (`01JB000000000000000MEM0002`), an agent owned by @sam | `agent`: routes marked **agent**; reads everything, writes only to its own tasks and sessions |

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

- **Every route in the contract**, with the `ApiError` body and status for each failure (400, 401,
  403, 404, 409, 503). Task routes accept an id, a prefixed id (`tsk_…`) or a key (`PAP-4`).
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
- **Editing tasks** (`PATCH /v1/tasks/{id-or-key}`): every rule in the contract (title, labels,
  workstream, `blocked_by` with cycles as 409, dates). `task_updated` carries only the fields that
  changed, and a patch that changes nothing emits nothing.
- **Briefs.** `PUT` stores `next`, and `brief_accepted` carries it. Accepting the pending proposal
  unchanged (the fixture's revision 15 for PAP is one) copies its receipts, and the brief stays the
  back office's.
- **The event log.** The fixture's 15 events are revisions 1–15; every change appends an event with
  a new ULID. `GET /v1/events` pages it (`before` is exclusive, `at_start` says whether older
  matching events exist) and filters by project, workstream, task or session.
- **`GET /v1/stream`**: `hello`, then the missed events when `since` is behind, then live events
  batched over 60 ms, and a `ping` every 20 s.
- **Simulated sessions.** A dispatch or a new session starts in `starting`; about 1.5 s later it
  turns `working`, and a dispatched agent moves its task to in progress. `send` records the prompt,
  then a canned reply and `turn_ended` follow about 0.8 s later. Escape, Ctrl-C and `interrupt`
  stop the turn; `end` emits `session_ended`. Answering an ask resumes its waiting session.
- **Transcripts** for all six demo sessions, paged tail-first with `before` and `limit`. SES0001
  is the Claude fixture transcript (its offsets are the file's real record offsets), and the
  receipts in the demo workspace point at real items.
- **Terminals.** `GET /v1/sessions/{id}/terminal` replays a short ANSI screen, echoes keystrokes,
  accepts `{"type":"resize"}` and ignores unknown control types (malformed JSON closes with 1007),
  sends `{"type":"truncated"}` before the replay for sessions that ran over a day, and sends
  `{"type":"exit"}` when the session ends. Sessions on the unreachable GPU box answer 503.

## What it does not do

- It runs no agents and reads no real transcripts; replies and terminal screens are canned.
- Nothing is saved: restart the server to get the demo workspace back.
- No back office or tracker sync: briefs are only proposed by the fixture, dispatches never finish
  on their own, workstream health never changes by itself, and mentions do not create asks.
- A resize changes nothing, and `model`, `persona` and `permission_mode` on a new session are only
  checked, not used (the contract says they are not echoed on `Session`).
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
| `src/server.ts` | Entry point: HTTP, CORS, bodies, WebSocket upgrades. Exports `startServer({ port })`. |
| `src/routes.ts` | The routes, tokens and scope rules. |
| `src/live.ts` | The event stream and terminal sockets. |
| `src/state.ts` | In-memory state, the event log and its revisions. |
| `src/simulate.ts` | Simulated session liveness. |
| `src/transcripts.ts` | Canned transcripts and paging. |
| `src/ws.ts` | A minimal WebSocket server (RFC 6455). |
| `src/types.ts` | Wire types mirroring `crates/protocol`. |
| `src/rules.ts` | `can_move` and date checks ported from `model.rs`. |
| `src/validate.ts` | `ApiError` failures and request validation. |
| `src/ulid.ts` | Time-ordered ULIDs. |

`tsconfig.json` is for editors only (it needs TypeScript 5.8+ and `@types/node`, which this package
does not install); nothing is compiled.
