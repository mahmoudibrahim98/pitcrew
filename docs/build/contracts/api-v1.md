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
| `conflict` | 409 | An allowed caller, but the rules say no: a move `can_move` rejects, dispatching a done or canceled task, a project key already in use, a `blocked_by` cycle |
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
| `POST /v1/projects` | `NewProject` → `Project` (201) | See below. `409 conflict` if the key is in use. Emits `project_created`. |
| `GET /v1/workstreams?project=` | → `Workstream[]` | |
| `GET /v1/workstreams/{id}` | → `Workstream` | |
| `POST /v1/workstreams` | `NewWorkstream` → `Workstream` (201) | See below. `404 not_found` for an unknown project. Emits `workstream_created`. |
| `PATCH /v1/workstreams/{id}` | `{ "status"?, "health"? }` → `Workstream` | Emits `workstream_changed`. |

`NewProject`, `NewWorkstream` and `NewTask` are Rust types in `crates/protocol/src/api.rs`.

`NewProject`: `{ "key": ProjectKey, "name": String, "lead"?: MemberId, "members"?: MemberId[],
"status"?: ProjectStatus, "start"?: Date, "due"?: Date, "root"?: Location }`. The hub assigns `id`;
`external` starts empty.
- `key` is a `ProjectKey`: 2 to 10 characters, an uppercase ASCII letter, then uppercase letters or
  digits (`PAP`, `TL2`). Any other key is `400 invalid`; a key another project has is
  `409 conflict`. Task keys are `<key>-<n>`, starting at 1.
- `name` must not be blank.
- `lead` defaults to the caller. `members` defaults to the lead alone; the lead is always a member
  (put first when the list leaves it out), and duplicates are dropped. An unknown member is `400`.
- `status` defaults to `in_progress`.
- Dates are `YYYY-MM-DD`, and `start` ≤ `due` when both are set (`400`).
- `root` names a known machine and a non-empty path (`400`).

`NewWorkstream`: `{ "project": ProjectId, "name": String, "status"?: WorkstreamStatus,
"locations"?: Location[] }`. The hub assigns `id`; `health` starts `on_track` and `external` empty.
- An unknown `project` is `404 not_found`, although it is in the body.
- `name` must not be blank. `status` defaults to `active`. Each location names a known machine and
  a non-empty path (`400`).

### Tasks

| Method and path | Body → response | Notes |
|---|---|---|
| `GET /v1/tasks?project=&workstream=&assignee=&status=` | → `Task[]` | All filters optional; `status` may repeat. **agent** |
| `GET /v1/tasks/{id-or-key}` | → `Task` | **agent** |
| `PATCH /v1/tasks/{id-or-key}` | `TaskPatch` → `Task` | See "Editing a task". Emits `task_updated` with the fields that changed, or nothing. |
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

**Editing a task.** `TaskPatch` (in `crates/protocol/src/model.rs`):
`{ "workstream"?: WorkstreamId | null, "title"?: String, "description"?: String,
"priority"?: Priority, "labels"?: String[], "start"?: Date | null, "due"?: Date | null,
"blocked_by"?: TaskId[], "accept_auto"?: bool }`.
- A field left out is unchanged. `null` clears `workstream`, `start` and `due`; on the other fields
  `null` is the same as leaving the field out. `labels` and `blocked_by` replace the whole list.
- Status, assignee and subtasks have their own routes. Like any unknown field, they are ignored.
- The hub checks the whole patch before it changes anything:

| Rule | Error |
|---|---|
| `title` is 1 to 500 characters (Unicode code points) after trimming whitespace; the trimmed title is stored | `400 invalid` |
| `labels` are trimmed and deduplicated (the first stays); then each is 1 to 64 characters, and there are at most 32 | `400 invalid` |
| `workstream` is a known workstream of the task's project | `400 invalid` |
| `blocked_by` holds ids of existing tasks, never the task's own id; duplicates are dropped | `400 invalid` |
| `blocked_by` creates no cycle: no task in it already waits on this task, directly or through other tasks | `409 conflict` |
| `start` and `due` are `YYYY-MM-DD`, and `start` ≤ `due` when the task will have both (a new `start` is checked against the current `due`, and the other way round) | `400 invalid` |
| `priority` is a `Priority`, `accept_auto` a boolean, and the other fields strings or lists of strings | `400 invalid` |

- The hub then compares the checked values with the task's. `task_updated` carries only the fields
  that differ (lists compare in order), with `null` for a field it cleared. A patch that changes
  nothing, `{}` included, returns the task and emits nothing.

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
| `PUT /v1/briefs/{project\|workstream}/{id}` | `{ "text", "next"?, "pinned" }` → `Brief` | A person's brief, or a proposal they accept. See "Briefs". Emits `brief_accepted`. |
| `GET /v1/events?before=&limit=&project=&workstream=&task=&session=` | → `{ "events": Event[], "from_rev": u64, "to_rev": u64, "at_start": bool }` | See below. |

**Briefs.** The brief in force for a project or workstream is the one its newest `brief_accepted`
put there. Its **pending proposal** is the newest `brief_proposed` for that target, if it is newer
(a higher `rev`) than that `brief_accepted`, or if the target has no `brief_accepted` yet.
- `PUT` stores `text`, `next` and `pinned`, and `brief_accepted` carries all three. Like
  `brief_proposed`, it leaves `next` out when there is none.
- **Accepting a proposal.** When the `PUT`'s `text` and `next` both equal the pending proposal's
  (a missing `next` equals only a missing `next`), the hub copies the proposal's `receipts` into
  `brief_accepted`, and the brief's `source` is `back_office`.
- Any other `PUT` is the person's own text: `source` is `person`, and it has no receipts.
- **Keep current** is a `PUT` of the current text. It needs no new event kind: the accepted brief is
  then newer than the proposal, so nothing is pending. The brief becomes the person's.
- A `brief_accepted` written by an agent (the back office applying a brief itself) also has
  `source: back_office`. Projections rebuild `Brief` from the log with these same rules, in
  revision order; its `receipts` are those of the `brief_accepted`.

**Who may answer an ask:**
- A `device` token: asks addressed to that person, or to an agent that person owns.
- An `agent` token: only asks addressed to itself, and only of kind `question` or `mention`.
- `decision`, `approval` and `review` always need a `device` token.

**Activity paging** (`GET /v1/events`, response type `EventsPage`): events oldest first within
the page, the newest page when `before` is absent. `before` is an exclusive revision. `from_rev`
and `to_rev` are the revisions of the first and last returned events; with filters they need not
be contiguous. Pass `from_rev` as `before` for the previous page. Default limit 100, max 500;
`limit=0` is 400.
- **Only `at_start` ends paging.** With filters the hub scans a bounded window per request, so a
  page may hold fewer than `limit` events, even none. An empty page that is not at the start has
  `to_rev = 0` and `from_rev` = where the scan stopped; keep paging from it.
- **Filters match events that name the entity directly:** `session` matches events carrying that
  session id, and `task` those carrying that task id. Events that reach it only through a link
  (a turn in a session linked to the task; `dispatch_finished`, which names the dispatch) are not
  included until the hub has an index for them. `project` and `workstream` answer `400 invalid`
  until then.

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
  keys it touches (e.g. `task_moved` → that task, its lists and its workstream; `task_updated` →
  that task, its lists, and its old and new workstream).

## Terminals: `GET /v1/sessions/{id}/terminal?cols=&rows=&from=` (WebSocket, device tokens)

- `cols` and `rows` are 1..=1000 (400 outside). `from` (default 0) is the byte offset to start
  from: a reconnecting client passes the number of output bytes it has received.
- Server → client **binary** frames are terminal output: first what the runtime's buffer holds
  from `from`, then live output.
- Server → client **text** frames:
  - `{"type":"truncated","from":<offset>}`: the next binary byte is at `offset`, because the
    buffer no longer held the requested bytes (or `from` was past the end). It usually comes
    before the replay, but **may arrive at any time**;
  - `{"type":"exit"}` when the program ends, after its last output; a terminal that disappears
    counts as ended.
- Client → server **binary** frames are keystrokes, written as-is.
- Client → server **text** frames are control messages: `{"type":"resize","cols":120,"rows":40}`
  (1..=1000). Unknown `type`s are ignored; malformed JSON or an invalid size closes with 1007.
- **Close codes:** 1000 after `exit`; 1007 malformed control; 1013 client too slow (reconnect
  with `from`); 1011 runtime failure; 1001 hub shutting down. The stream uses 1013 too slow
  (reconnect with `since`), 1001 source closed or hub shutting down, and 1011 failure.
- The mock echoes input back and replays a short canned screen.
