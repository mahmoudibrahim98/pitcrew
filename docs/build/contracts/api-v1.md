# API v1

The daemon's HTTP and WebSocket API, as the desktop UI and the `pitcrew` CLI use it.

- **Types** are the Rust types in `crates/protocol`. JSON field names are exactly the serde names;
  enums are `snake_case` strings; ids are bare 26-character ULIDs; times are UTC milliseconds.
- **The real server** is stream H (`crates/api`). **The mock** is `apps/mock-hub`, which serves
  `crates/fixtures/data/demo-workspace.json` and implements every route below with in-memory
  state. The UI streams build against the mock. If the two disagree, this document wins, and the
  one that is wrong gets fixed.
- **Changes** go through a contract change (`s/0/contract-…`). Adding an optional field or a new
  route is not breaking. Removing or renaming anything is, and bumps `PROTOCOL_VERSION`.

## Transport and auth

- The daemon listens on a **unix socket** (Windows: a **named pipe**), never on a TCP port by
  default. The desktop reaches remote daemons through an SSH tunnel to that socket. The mock hub
  listens on `127.0.0.1:47317` (override with `PORT`) for browser development, and answers only
  requests addressed to `localhost`, `127.0.0.1` or `*.localhost` (DNS-rebinding guard).
- Every route except `GET /v1/host/info` needs `Authorization: Bearer <token>`. **Tokens never go
  in a query string.**
- A WebSocket cannot set headers from a browser, so WebSocket routes take the token as a
  subprotocol: `Sec-WebSocket-Protocol: pitcrew.v1, pitcrew.bearer.<token>`. The server answers
  with `pitcrew.v1`.
- Two scopes (`TokenScope`):
  - `device`, a person's desktop: every route.
  - `agent`, an agent or hook: only routes marked **agent** below.
    - **Reads** on those routes see the whole workspace, so agents can coordinate.
    - **Writes** are limited to the agent's **own** tasks (it is the assignee, or holds the task's
      active dispatch) and its own sessions. A write on anything else is `403 forbidden`.
  - The hub stamps `author` (the caller) and, for agents, `on_behalf_of` (the owner) from the
    token, never from the body.
- Mock tokens: `dev-device-token` (acts as `@sam`) and `dev-agent-token` (acts as `@writer`).

## Errors

Every failure returns an `ApiError` body, `{"code": "…", "message": "…"}`:

| `code` | HTTP | When |
|---|---|---|
| `unauthorized` | 401 | No token, or an unknown one |
| `forbidden` | 403 | The token's scope does not allow it, or an agent writes to something not its own |
| `not_found` | 404 | No such resource **in the path**; also an unknown route or method |
| `conflict` | 409 | An allowed caller, but the rules say no: a move `can_move` rejects, dispatching a done or canceled task |
| `invalid` | 400 | Malformed body or query, unknown enum value, an unknown id **in the body**, a body over 1 MiB, a WebSocket route called without an upgrade |
| `unavailable` | 503 | The session's machine is unreachable |
| `internal` | 500 | Anything else |

## Routes

Ids in paths are bare ULIDs (the `tsk_…` display form is also accepted). Task routes also accept
the task key (`PAP-4`).

### Host and workspace

| Method and path | Body → response | Notes |
|---|---|---|
| `GET /v1/host/info` | → `HostInfo` | No auth. Check `protocol_min ≤ yours ≤ protocol` before anything else. |
| `GET /v1/me` | → `Member` | The token's member. **agent** |
| `GET /v1/workspace` | → `{ "workspace": Workspace, "rev": u64 }` | `rev` is the current event revision. |
| `GET /v1/machines` | → `Machine[]` | |
| `GET /v1/members` | → `Member[]` | **agent** |
| `GET /v1/personas` | → `Persona[]` | |
| `GET /v1/teams` | → `Team[]` | |

### Projects and workstreams

| Method and path | Body → response | Notes |
|---|---|---|
| `GET /v1/projects` | → `Project[]` | |
| `GET /v1/projects/{id}` | → `Project` | |
| `GET /v1/workstreams?project=` | → `Workstream[]` | |
| `GET /v1/workstreams/{id}` | → `Workstream` | |
| `PATCH /v1/workstreams/{id}` | `{ "status"?, "health"? }` → `Workstream` | Emits `workstream_changed`. |

### Tasks

| Method and path | Body → response | Notes |
|---|---|---|
| `GET /v1/tasks?project=&workstream=&assignee=&status=` | → `Task[]` | All filters optional; `status` may repeat. **agent** |
| `GET /v1/tasks/{id-or-key}` | → `Task` | **agent** |
| `POST /v1/tasks` | `NewTask` → `Task` (201) | See below. Emits `task_created`. |
| `POST /v1/tasks/{id}/move` | `{ "to": TaskStatus }` → `Task` | The mover comes from the token: `device` → `person`; `agent` → `agent` (own tasks only, else 403). `409 conflict` if `can_move` is false. Emits `task_moved`. **agent** |
| `POST /v1/tasks/{id}/assign` | `{ "assignee": MemberId \| null }` → `Task` | The key is required; `null` unassigns. Emits `task_assigned`. |
| `PUT /v1/tasks/{id}/subtasks` | `Subtask[]` → `Task` | A `device` token replaces the whole list. An `agent` token (own task) replaces only **its own** `agent_plan` lines and keeps every other line. Emits `subtasks_replaced` with the full resulting list. **agent** |
| `POST /v1/tasks/{id}/comments` | `{ "text": String, "mentions": MemberId[] }` → `Event` (201) | Emits `comment_posted`. **agent** |
| `POST /v1/tasks/{id}/dispatch` | `{ "agent": MemberId, "brief"?: String, "machine"?: MachineId }` → `Dispatch` (202) | See "Dispatch" below. |

`NewTask`: `{ "project": ProjectId, "workstream"?: WorkstreamId, "title": String,
"description"?: String, "status"?: TaskStatus (default "todo"), "priority"?: Priority,
"assignee"?: MemberId, "labels"?: String[], "due"?: Date }`. The hub assigns `id` and the next
`key` in the project.

**Dispatch.** Starts a session for the agent on the task:
- `409 conflict` if the task is done or canceled.
- If the task has no assignee, it is assigned to the agent (`task_assigned`).
- The machine and folder default to the workstream's first location, then the project's root,
  then the hub's own machine.
- Emits `dispatch_started`, then `session_discovered` (state `starting`, `link_basis: dispatch`).
  When the session starts working, the task moves to in progress (`task_moved`, mover `agent`).

### Sessions

| Method and path | Body → response | Notes |
|---|---|---|
| `GET /v1/sessions?machine=&workstream=&task=&state=` | → `Session[]` | |
| `GET /v1/sessions/{id}` | → `Session` | |
| `GET /v1/sessions/{id}/transcript?before=&limit=` | → `TranscriptPage` | See "Transcript paging". |
| `POST /v1/sessions` | `StartSession` → `Session` (202) | Starts a new session. Emits `session_discovered`. |
| `POST /v1/sessions/{id}/send` | `{ "text": String }` → 204 | Types text and presses Enter. |
| `POST /v1/sessions/{id}/keys` | `{ "keys": Key[] }` → 204 | e.g. `["escape"]`. |
| `POST /v1/sessions/{id}/interrupt` | → 204 | |
| `POST /v1/sessions/{id}/end` | `{ "mode": "graceful" \| "kill" }` → 204 | Emits `session_ended` when it has ended; that event **is** the change to state `ended` (no separate `session_state_changed`). |
| `POST /v1/sessions/{id}/link` | `{ "workstream"?: WorkstreamId, "task"?: TaskId }` → `Session` | A manual link (`link_basis: "manual"`). Emits `session_linked`. |

`StartSession`: `{ "machine": MachineId, "engine": Engine, "cwd": String, "agent"?: MemberId,
"task"?: TaskId, "brief"?: String, "persona"?: PersonaId, "model"?: String,
"permission_mode"?: PermissionMode }`. `persona`, `model` and `permission_mode` are launch options
and are not echoed on `Session`. With a `task`, the session is linked with `link_basis: "manual"`.

**Transcript paging.** Tail-first: without `before`, the newest page; pass a page's `from` as
`before` to get the previous one. `limit` counts items (default 200, max 1000). Pages hold
**whole records**, so a page can exceed `limit` when one record yields many items. `at_start` is
true when nothing older exists, even if `from` is greater than 0 (a transcript's first record need
not start at byte 0).

### Asks, briefs, activity

| Method and path | Body → response | Notes |
|---|---|---|
| `GET /v1/asks?to=&state=` | → `Ask[]` | The Inbox is `?to=<me>&state=open`. **agent** |
| `POST /v1/asks` | `{ "kind", "to", "title", "body"?, "options"?, "task"?, "session"?, "receipts"? }` → `Ask` (201) | Emits `ask_raised`. **agent** |
| `POST /v1/asks/{id}/answer` | `{ "option"?: usize, "text"?: String }` → `Ask` | See below. Emits `ask_answered`. **agent** |
| `GET /v1/briefs` | → `Brief[]` | |
| `PUT /v1/briefs/{project\|workstream}/{id}` | `{ "text", "next"?, "pinned" }` → `Brief` | A person's edit (`source: "person"`). Emits `brief_accepted`. |
| `GET /v1/events?before=&limit=&project=&workstream=&task=&session=` | → `{ "events": Event[], "from_rev": u64, "to_rev": u64, "at_start": bool }` | See below. |

**Who may answer an ask:**
- A `device` token: asks addressed to that person, or to an agent that person owns.
- An `agent` token: only asks addressed to itself, and only of kind `question` or `mention`.
- `decision`, `approval` and `review` always need a `device` token.

**Activity paging** (`GET /v1/events`): events oldest first within the page, the newest page
when `before` is absent. `before` is an exclusive revision. `from_rev` and `to_rev` are the
revisions of the first and last returned events (both 0 for an empty page); with filters they
need not be contiguous. `at_start` is true when no older matching event exists. Pass `from_rev`
as `before` for the previous page. Default limit 100, max 500.

### Hooks

| Method and path | Body → response | Notes |
|---|---|---|
| `POST /v1/hooks/{engine}/{event}` | the CLI's hook payload (a JSON object, ≤ 1 MiB) → 202 | Sent by `pitcrew hook`. `engine` is an `Engine`; `event` is the CLI's own event name (e.g. `SessionStart`, `Stop`), matching `[A-Za-z][A-Za-z0-9_-]{0,63}`. The hub uses it for session state and never blocks the caller. **agent** |

## Live updates: `GET /v1/stream?since=<rev>` (WebSocket, device tokens)

- Text frames, each one `StreamFrame` JSON.
- The first frame is `{"type":"hello","rev":N,"log":"<id>"}`. If `since` is given and older than
  `N`, the server then sends the missed events as `events` frames before live ones. A client that
  reconnects with its last `to_rev` receives exactly what it missed.
- **`log` identifies the hub's event log.** It is created with the store and never changes.
  Revisions only count within one log: if `log` differs from the one the client's cache came
  from, or `since` is newer than `N`, the client must drop its cached state and refetch.
- `events` frames carry `from_rev..=to_rev` and the events in order. Small changes are batched
  over 50–100 ms.
- `{"type":"ping","at":…}` every 20 s. A client that sees nothing for 60 s reconnects.
- Agents and hooks use HTTP; the stream is for `device` tokens in v1.
- **Client rule:** keep server state in TanStack Query, and on each event invalidate exactly the
  keys it touches (e.g. `task_moved` → that task, its lists and its workstream).

## Terminals: `GET /v1/sessions/{id}/terminal?cols=&rows=` (WebSocket, device tokens)

- Server → client **binary** frames are terminal output. The first frames replay the runtime's
  buffer, then output is live.
- Server → client **text** frames:
  - `{"type":"truncated","from":<offset>}`, sent **before** the replay when the buffer no longer
    held the start;
  - `{"type":"exit"}` when the program ends.
- Client → server **binary** frames are keystrokes, written as-is.
- Client → server **text** frames are control messages: `{"type":"resize","cols":120,"rows":40}`.
  Unknown `type`s are ignored; malformed JSON closes the socket with code 1007.
- The mock echoes input back and replays a short canned screen.
