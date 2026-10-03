# API v1

The daemon's HTTP and WebSocket API, as the desktop UI and the `pitcrew` CLI use it.

- **Types** are the Rust types in `crates/protocol`. JSON field names are exactly the serde names;
  enums are `snake_case` strings; ids are bare 26-character ULIDs; times are UTC milliseconds.
- **The real server** is stream H (`crates/api`). **The mock** is `apps/mock-hub`, which serves
  `crates/fixtures/data/demo-workspace.json` and implements every route below with in-memory
  state (recaps come from `demo-recaps.json` beside it). The UI streams build against the mock. If the two disagree, this document wins, and the
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
| `GET /v1/me` | → `Member` | The token's member. **agent** `404` before setup (see below). |
| `GET /v1/workspace` | → `{ "workspace": Workspace, "rev": u64, "setup_needed": bool }` | `rev` is the current event revision. `setup_needed` is `true` while the workspace has no person (a fresh hub); omitted means `false`. |
| `POST /v1/setup` | `Setup` → `{ "workspace": Workspace, "me": Member, "machine": Machine }` | The first run (see below). Device tokens only. |
| `GET /v1/machines` | → `Machine[]` | |
| `GET /v1/members` | → `Member[]` | **agent** |
| `GET /v1/personas` | → `Persona[]` | |
| `GET /v1/teams` | → `Team[]` | |

#### The first run: `POST /v1/setup`

A fresh hub has a device token but no person, no machine and no name. The desktop's onboarding
(or `pitcrewd init`) sets it up once:

```json
{ "workspace_name": "Demo Lab",
  "person": { "name": "Sam Rivera", "handle": "@sam" },
  "machine_name": "This laptop" }
```

- **The person is the device token's member.** The token already acts as a member id that nothing
  knows. Setup appends `member_added` for that id (kind `human`, no owner) with this name and
  handle, so the token and `GET /v1/me` mean this person from then on.
- **The machine** is appended with `machine_added`: kind `local`, liveness `live`. It is the hub's
  own machine, and the one its runner watches.
- **Both events go in one append,** authored by the person. The workspace's name is kept by the
  hub, not in the event log, and `GET /v1/workspace` returns it from then on.
- **Validation:**
  - `workspace_name` is 1–80 characters;
  - `person.name` is 1–80 characters;
  - `person.handle` is `@` followed by 1–32 of `a-z 0-9 _ -`;
  - `machine_name` is 1–60 characters.
  - The three names are counted in Unicode code points after trimming whitespace, and stored
    trimmed. Whitespace is what JavaScript's `String.prototype.trim` removes, so the hub and the
    mock agree. The handle is not trimmed.
  - None of them may contain control characters. Anything else is `400 invalid`.
- **Once only:** `409 conflict` when the workspace already has a person, or when the handle is
  taken. `@office` is reserved for the back office and is always taken. An agent token gets `403`.
- **After setup,** the hub starts what needed a person: the back office, and the runner on this
  machine. `setup_needed` becomes `false` at once. The answer may come a moment before they have
  started: `GET /v1/host/info`'s roles show the runner once it runs.
- **The mock** starts with the demo's person, so `setup_needed` is `false` and setup answers
  `409`. Start it with `PITCREW_MOCK_FRESH=1` for an empty workspace (no members, machines or
  work) whose setup succeeds once.

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

**Dispatch.** Starts a session for the agent on the task. People only (`403` for an agent), and
**only their own agents**: a person may dispatch an agent whose `owner` is that person, never
another person's agent or one with no owner (explicit sharing may come later).
- Refusals, in this order, with nothing recorded: `404` an unknown task; `400` an unknown agent or
  machine, a person named as the agent, or a brief (the one given, or the task's description or
  title it defaults to) longer than 64 KiB; `403 forbidden` an agent the caller does not own;
  `409 conflict` if the task is done or canceled, or the agent already holds an active dispatch on
  it; `503 unavailable` when no machine can run it (none is live, the hub has no runner attached
  yet, or the machine's runner cannot be reached).
- If the task has no assignee, it is assigned to the agent (`task_assigned`).
- The machine and folder default to the workstream's first location, then the project's root,
  then the hub's own machine (in the home folder, `~`).
- Emits `dispatch_started`, then `session_discovered` (state `starting`, `link_basis: dispatch`),
  then starts the agent's CLI there. The runner reports the CLI as **that session**: its
  transcript takes the dispatch's session id, and the session is never discovered a second time
  under another. The CLI runs with an **agent** token for the dispatched agent, acting for its
  owner (its hooks and `pitcrew` use it), never a person's token.
- If the CLI cannot start, the dispatch finishes at once (`dispatch_finished`, outcome `failed`,
  the reason as its summary), the session ends (`session_ended`), and the dispatch answers `409`
  (the runner refused: a folder that is not one, a permission mode it does not allow, or a Codex
  or OpenCode start in a folder where another, for a dispatch or a start for an agent or task,
  still waits for its transcript: the two could not be told apart), `503` (the machine or its
  terminals cannot be reached) or `500`.
- **The task moves itself** (the hub, following what the runner reports):
  - when the session first reports `working`, the task moves to in progress (`task_moved`, mover
    `agent`, authored by the agent for its owner), if an agent may move it there;
  - when the agent reports the work done, by moving its task to review (`pitcrew report <task>
    --review`), the dispatch finishes as `succeeded` (`dispatch_finished`) in the same
    transaction as the move. A task a person already moved to review counts as that report: the
    agent's move answers `200` with the task, unchanged, and the dispatch succeeds. The back
    office moves a task still in progress to review when a dispatch succeeds
    (`dispatch_to_review`);
  - when the session ends without that report, the dispatch finishes as `canceled`, summary "The
    session ended without a report." (it stopped work), or as `failed`, summary "The session
    ended before its CLI started.", if the runner never reported the session;
  - when its CLI never started (the hub stopped between the dispatch and the start, or the CLI
    ended before its transcript appeared), or a Codex or OpenCode CLI's transcript did not appear
    within 15 minutes of its start (past that, no transcript is matched to it by folder), the hub
    finishes the dispatch as `failed` and ends the session, at its next start or once it sees
    that. A start still under way is never taken for one that did not start.
  A person can always move the task themselves; the session's later turns do not move it back. A
  session the hub ended stays ended: the runner re-stating it (`session_discovered`) does not
  bring it back.

### Sessions

| Method and path | Body → response | Notes |
|---|---|---|
| `GET /v1/sessions?machine=&workstream=&task=&state=` | → `Session[]` | |
| `GET /v1/sessions/{id}` | → `Session` | |
| `GET /v1/sessions/{id}/transcript?before=&limit=` | → `TranscriptPage` | See "Transcript paging". |
| `POST /v1/sessions` | `StartSession` → `Session` (202) | Starts a new session. Emits `session_discovered`. See below. |
| `POST /v1/sessions/{id}/send` | `{ "text": String }` → 204 | Types text and presses Enter. |
| `POST /v1/sessions/{id}/keys` | `{ "keys": Key[] }` → 204 | e.g. `["escape"]`. |
| `POST /v1/sessions/{id}/interrupt` | → 204 | |
| `POST /v1/sessions/{id}/end` | `{ "mode": "graceful" \| "kill" }` → 204 | Emits `session_ended` when it has ended; that event **is** the change to state `ended` (no separate `session_state_changed`). |
| `POST /v1/sessions/{id}/link` | `{ "workstream"?: WorkstreamId, "task"?: TaskId }` → `Session` | A manual link (`link_basis: "manual"`). Emits `session_linked`. |

`StartSession`: `{ "machine": MachineId, "engine": Engine, "cwd": String, "agent"?: MemberId,
"task"?: TaskId, "brief"?: String, "persona"?: PersonaId, "model"?: String,
"permission_mode"?: PermissionMode }`. `persona`, `model` and `permission_mode` are launch options
and are not echoed on `Session`. With a `task`, the session is linked with `link_basis: "manual"`.
- With an `agent` or a `task`, the hub stores the session before its CLI starts
  (`session_discovered`, state `starting`, the agent named, linked to the task) and answers it
  (`202`) once the CLI has started; the runner reports the CLI under that id, as for a dispatch,
  and a session run as an agent gets that agent's token, never a person's. `400` for an unknown
  agent or task, or a person named as the agent; `403 forbidden` for an agent the caller does not
  own (as for a dispatch, a person runs only their own agents). If the runner refuses or fails
  the start, the session ends (`session_ended`) and the start answers why. If the runner does not
  answer in time, the start answers `503` and the session stays `starting`: the hub ends it later
  only if its CLI did not start, as for a dispatch. It moves no task: only a dispatch does.
- Without them, the start answers once the runner has found the CLI's transcript.

**Transcript paging.** Tail-first: without `before`, the newest page; pass a page's `from` as
`before` to get the previous one. `limit` counts items (default 200, max 1000). Pages hold
**whole records**, so a page can exceed `limit` when one record yields many items. `at_start` is
true when nothing older exists, even if `from` is greater than 0 (a transcript's first record need
not start at byte 0).
- A session whose machine's runner has not indexed a transcript for it (a demo session, or a
  dispatched one whose transcript does not exist yet) answers **an empty page** with
  `at_start: true` (`items` empty, `from` and `to` 0), as for a session without a transcript.
- A transcript that is gone (deleted) or cannot be read, and a read that is busy or does not finish
  in time, answer `503 unavailable`; the message says which, and names no path. So do a session on
  a machine the hub cannot reach and, on a hub without a runner, every session.
- An unknown session is `404`; a `limit` of 0, or a `before` or `limit` that is not a whole number,
  is `400`.

### Asks, briefs, activity

| Method and path | Body → response | Notes |
|---|---|---|
| `GET /v1/asks?to=&state=` | → `Ask[]` | The Inbox is `?to=<me>&state=open`. **agent** |
| `POST /v1/asks` | `{ "kind", "to", "title", "body"?, "options"?, "task"?, "session"?, "receipts"? }` → `Ask` (201) | Emits `ask_raised`. **agent** |
| `POST /v1/asks/{id}/answer` | `{ "option"?: usize, "text"?: String }` → `Ask` | See below. Emits `ask_answered`. **agent** |
| `GET /v1/briefs` | → `Brief[]` | The briefs in force, each with its pending proposal in `proposal` when it has one. See "Briefs". |
| `PUT /v1/briefs/{project\|workstream}/{id}` | `{ "text", "next"?, "pinned" }` → `Brief` | A person's brief, or a proposal they accept. See "Briefs". Emits `brief_accepted`. |
| `GET /v1/events?before=&limit=&project=&workstream=&task=&session=` | → `{ "events": Event[], "from_rev": u64, "to_rev": u64, "at_start": bool }` | See below. |

**Briefs.** The brief in force for a project or workstream is the one its newest `brief_accepted`
put there. Its **pending proposal** is the newest `brief_proposed` for that target, if it is newer
(a higher `rev`) than that `brief_accepted`, or if the target has no `brief_accepted` yet.
- `Brief.proposal` (a `BriefProposal`: `{ "text", "next"?, "receipts", "at" }`, where `at` is the
  time of its `brief_proposed`) is the pending proposal, present exactly when there is one, so
  clients need not scan events for it. Accepting it, or keeping the current brief, clears it. A
  target with a proposal but no brief in force yet is not listed.
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
- **Filters combine:** an event must match every filter given. `session` and `task` match
  events that name that session or task: a `session` or `task` field, at any depth, holding the
  id or an object with that `id`.
- **With the hub's activity index** (a hub running the work model has one), filters also follow
  links as they were when each event happened:
  - `task` also matches the events of sessions linked to the task (turns, tool runs, file edits),
    and the `dispatch_finished` and `ask_answered` of its dispatches and asks;
  - `session` also matches the `dispatch_finished` and `ask_answered` of its dispatches and asks;
  - `project` and `workstream` match events about the project or workstream, including those of
    its tasks and their sessions.
- **Without the index**, `project` and `workstream` answer `400 invalid`, and `task` and
  `session` match only events that name them.

### Recaps

| Method and path | Body → response | Notes |
|---|---|---|
| `GET /v1/recaps/blocks?session=&task=&workstream=&project=&before=&limit=` | → `BlocksPage` | Activity blocks, newest first, each with its line. See "Recap blocks". |
| `GET /v1/recaps/days?workstream=\|project=&tz=&before=&limit=` | → `DaysPage` | Day paragraphs, newest day first. See "Recap days". |

Both need a device token, like the activity log they summarise. The types are in
`crates/protocol/src/recap.rs`; the recap engine (`crates/recap`, stream F) computes them.

**Recap types.**
- `BlocksPage`: `{ "blocks": RecapBlock[], "at_start": bool }`, where `RecapBlock` is
  `{ "block": Block, "line": Summary }`.
- `DaysPage`: `{ "days": DayRecap[], "at_start": bool }`, where `DayRecap` is
  `{ "workstream"?: WorkstreamId, "date": Date, "blocks": EventId[], "summary": Summary }` and
  `blocks` are the ids of the blocks the paragraph covers, by start.
- `Block` is a burst of one session's work, or of one workstream's (or project's) work outside any
  session, with no pause longer than the engine's gap (20 minutes):
  `{ "id": EventId, "last": EventId, "key": BlockKey, "start", "end", "session"?, "workstream"?,
  "project"?, "tasks": TaskId[], "agent"?: MemberId, "actors": MemberId[], "counts": Counts,
  "files": FileTouch[], "files_omitted", "facts": Fact[], "facts_omitted",
  "tool_receipts": Receipt[], "turn_receipts": Receipt[] }`.
  - `id` is its first event's id and `last` its last's; `start` and `end` are the times of its
    earliest and latest events.
  - `BlockKey` is `{ "kind": "session" | "workstream" | "project", "id" }`.
  - `Counts` has `events`, `tools_run`, `tools_failed`, `file_edits`, `lines_added`,
    `lines_removed`, `turns`, `asks_raised`, `asks_answered`, `task_moves` and `comments`.
    `FileTouch` is `{ "path", "edits", "added", "removed", "receipts" }`.
  - Lists are capped (8 tasks, 8 actors, 20 files, 24 facts, with `files_omitted` and
    `facts_omitted` counting the rest); `counts` never are.
- `Fact`: `{ "by": MemberId, "at", "kind": FactKind, "receipts": Receipt[] }`. `FactKind` is
  tagged by `type`, with its fields beside the tag: `session_started`, `session_linked`,
  `session_waiting`, `session_ended`, `dispatch_started`, `dispatch_finished`, `task_created`,
  `task_moved`, `task_assigned`, `plan_updated`, `checks` (`check` is `tests`, `lint` or `build`),
  `job_diverged`, `ask_raised`, `ask_answered`, `commented`, `decision_recorded`,
  `workstream_created`, `workstream_changed`, `brief_accepted`.
- `Summary`: `{ "text": String, "spans": Span[] }`, where `Span` is
  `{ "range": { "start": usize, "end": usize }, "receipts": Receipt[] }`.

**Spans are UTF-8 byte ranges.** `range.start` and `range.end` are byte offsets into the UTF-8
encoding of `text`, on character boundaries, with `start < end`. Spans come in order and never
overlap; the text between them is only the punctuation and spaces that join clauses. Every span
has at least one receipt. JavaScript strings are UTF-16, so the UI converts before slicing:
`text.slice(start, end)` is wrong as soon as the text holds a character outside ASCII (`−`, `é`, an
emoji). Slice the bytes (`new TextEncoder().encode(text).subarray(start, end)`, then decode), or
map byte offsets to string indices once per summary.

**Recap blocks** (`GET /v1/recaps/blocks`):
- Newest first, by block id (a ULID, so in the order the blocks began). `before` is a block id,
  exclusive: pass the last block's `id` for the previous page. Any well-formed event id works as
  `before`.
- `limit` defaults to 50; more than 200 counts as 200; `limit=0` is 400.
- **Only `at_start` ends paging.** A page may hold fewer than `limit` blocks, but a page that is
  not at the start holds at least one.
- **Filters combine** (a block must match every one given) and match the block's links: `session`
  its `session`, `task` one of its `tasks`, `workstream` its `workstream`, `project` its `project`.
  - The engine sets those links by following them as they were when the block's events happened,
    as the activity index does. A session's block carries the session's task, workstream and
    project, so `task` matches the turns, tool runs and edits of a session linked to the task, and
    the `dispatch_finished` and `ask_answered` the engine places in that session's blocks.
  - A block's links are those after its latest event. `tasks` lists the first 8 tasks touched, and
    `task` matches only those. `task` takes the task's id, not its key.
- Without filters: every block in the workspace, those of unlinked sessions included.

**Recap days** (`GET /v1/recaps/days`):
- Exactly one of `workstream` or `project`; neither or both is 400.
  - `workstream`: one entry per day with activity, the paragraph over the blocks whose
    `workstream` it is.
  - `project`: from the blocks whose `project` it is, one entry per workstream per day, plus one
    per day, without `workstream`, for the project's work outside any workstream (its tasks
    without one, and their sessions).
- `tz` is where days begin, in whole minutes east of UTC (`120` for UTC+2, `-300` for UTC−5), from
  −840 to 840; default 0. A block belongs to the day its `start` falls on at that offset. It is a
  fixed offset, not a time zone: send the offset in force for the days shown.
- Newest date first; within a date, the entry without a workstream first, then by workstream id.
- `before` is a date (`YYYY-MM-DD`), exclusive: pass the last entry's `date` for the previous page.
  `limit` counts dates, not entries: default 7, more than 30 counts as 30, `limit=0` is 400. A
  page holds every entry of its dates; a day without activity has no entry.
- `at_start` is true when no older day has an entry. A page that is not at the start holds at
  least one date.

**Recap errors.** `400 invalid` for a malformed id (neither a bare ULID nor its prefixed form),
date, `tz` or `limit`. An unknown id gives an empty page with `at_start: true`, as in the activity
route.

**What recaps are:**
- **Derived, never stored.** The hub computes recaps from the event log with the recap engine; no
  event records them. A block covers the log from `id` to `last`, and a day its blocks, so the hub
  may cache them by the range of events they cover and recompute when that range grows.
- **Open blocks grow.** While a block's last event is within the gap (20 minutes) of the newest
  activity, it may still grow: the same `id` comes back with a later `last`, more counts and
  facts, and a new line, and its day's paragraph changes with it. Older blocks no longer change.
- **Receipts are readable.** Every receipt in a block or a span points at an event
  (`GET /v1/events`), a transcript record (`GET /v1/sessions/{id}/transcript`), a job, a file or a
  commit that the caller can read.
- **Text is untrusted.** Lines and paragraphs are the engine's cleaned text (control and
  direction-changing characters removed, lengths capped), but they quote titles, paths and
  summaries that agents and people wrote. Render them as text, never as HTML or markdown.
- Today the hub writes lines and paragraphs with the engine's rules (`RuleSummarizer`). A model
  may write them later; the shape and the span rules stay the same.

**Live updates.** Recaps have no event or stream frame of their own: they change only when events
arrive, so the data layer refetches them when the activity they cover changes:
- Keys: `['recaps', 'blocks', filters]` and `['recaps', 'days', { workstream } | { project }, tz]`.
- On each `events` frame, for each event:
  - `machine_added`, `machine_liveness`, `persona_saved`, `team_saved`, `project_created` and
    `brief_proposed` are not activity: they change no recap.
  - `member_added` may rename someone a line names: invalidate every `['recaps']` key.
  - Any other event: find its scope as the activity route's filters would. That is the session,
    task, workstream and project it names, plus their parents from the cache (a session's task and
    workstream, a task's workstream and project, a workstream's project); `dispatch_finished` and
    `ask_answered` resolve through their dispatch or ask. For `session_linked`, and `task_updated`
    with a `workstream`, take both the old links and the new.
  - Invalidate the unfiltered blocks key, and every recap key whose `session`, `task`,
    `workstream` or `project` is in that scope.
- When the cache cannot resolve a link, invalidate every `['recaps']` key: always correct, only
  slower.

**The mock** serves both routes from `crates/fixtures/data/demo-recaps.json`, the engine's recaps
of the demo's events. They do not change with what you do through the mock, and it has days for
`tz=0` only: any other `tz` is `400 invalid`.

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
  that task, its lists, and its old and new workstream). Recaps have a rule of their own (see
  "Recaps", "Live updates").

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
- The server sends a WebSocket Ping every 20 s; a client that sends no Pong within 20 s is closed
  with 1013, like a slow one (reconnect with `from`).
- **Close codes:** 1000 after `exit`; 1007 malformed control; 1009 a client message over 1 MiB;
  1013 client too slow or no Pong (reconnect with `from`); 1011 runtime failure; 1001 hub
  shutting down. The stream uses 1013 too slow (reconnect with `since`), 1001 source closed or
  hub shutting down, 1011 failure, and 1009 a client message over 4 KiB.
- The mock echoes input back and replays a short canned screen. It does not send WebSocket Pings
  yet, and its message limit is 1 MiB on both sockets.
