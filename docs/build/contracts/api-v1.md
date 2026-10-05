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

## Files API

Brief `0-files-api` implements these device-only routes. Wire types live in
`crates/protocol/src/files.rs`, with generated TypeScript in `packages/protocol-ts`.

| Method and path | Request and response |
|---|---|
| `GET /v1/workstreams/{id}/files?loc=0&path=src` | List: `{ entries: [{ name, kind, size, modified_at }], truncated }` |
| `GET /v1/workstreams/{id}/files/content?loc=0&path=src/main.rs` | Read: `{ size, media_type, revision, encoding, content }` |
| `PUT /v1/workstreams/{id}/files/content?loc=0&path=src/main.rs` | Write: `{ revision, encoding, content }` to the same shape as read |

Using the workstream's location index avoids a second root registry and prevents
a client from supplying an arbitrary absolute root. `loc` is a required unsigned
decimal integer indexing `Workstream.locations`; only the hub's own local machine
is supported. Remote and WSL locations return `501 unsupported`. All three routes
require a device token; agent tokens receive `403` before any filesystem access.

`path` is required, relative, and uses `/` separators. The empty string lists the
root only; read and write require a nonempty path. Validate before disk access:
reject absolute paths, `..`, empty or `.` components, NUL and backslashes. Windows
also rejects drive, UNC and extended prefixes, colons (alternate data streams),
reserved device names even with extensions (`CON`, `PRN`, `AUX`, `NUL`, `COM1` to
`COM9`, `LPT1` to `LPT9`, including Windows' superscript-digit aliases), and trailing
dots or spaces that Windows normalises. Writes reject every `.git` component
(case-insensitively on Windows), including a final file named `.git`.

List entries have a UTF-8 name, `kind` of `file`, `folder` or `link`, byte `size`,
and UTC millisecond `modified_at` (null if unavailable). Access rules are unchanged
by ignore rules: listings additionally report `ignored` (boolean,
default false for older servers). This is a presentation hint from bounded, checked
`.gitignore` reads within the location root and its ancestors down to the listed
directory; it never changes read/write authorization. No global Git config or ignore
file outside the root is read. Malformed, unavailable, non-text or oversized ignore
files are skipped. At most 64 ignore files, 256 KiB of rules total and 4,096 rules
are considered. The explorer hides dot names and ignored entries by default, with
a show-hidden toggle. Recursive quick open uses the same lists, never follows links,
and stops at 128 directories or 5,000 examined entries, reporting incomplete results.
Non-UTF-8 names and special
files are omitted. Return at most 5,000 entries sorted by name in UTF-8 byte order;
`truncated` indicates that the complete directory could not be returned. Symbolic
links, junctions and all reparse points may be listed as links but never followed.
Walk each component without following links and check the opened handle's identity
before reading or writing. Protect ancestor directories against replacement too.

Read at most 8 MiB (8,388,608 bytes). `revision` is the lowercase hexadecimal
SHA-256 of the exact bytes; `encoding` is `utf8` for valid UTF-8 and otherwise
`base64` (canonical padded RFC 4648). `media_type` is a conservative type inferred
from the name, defaulting to `application/octet-stream`; content is always carried
inside JSON, never served as executable HTML. Responses carry `Cache-Control:
no-store` and `X-Content-Type-Options: nosniff`.

Write requires the revision returned by read, or JSON null meaning "must not
exist". Missing `revision` is invalid. A mismatch returns `409 conflict` with
`current_revision` (null for a missing target). Limit the JSON body to 12 MiB and
decoded content to 8 MiB. Serialize writes, recheck the target before replacement,
refuse targets with more than one hard link, preserve existing permissions, and
write to an exclusively created temporary file in the same directory before
atomic replacement. New files use private permissions. Refusal leaves the target
untouched; temporary files are cleaned up.

Before replacement, keep the old bytes under the daemon state directory's
`file-backups/`, keyed by a hash of root and relative path, never by client path
components. Retention: newest three backups per file, 64 MiB total,
oldest first eviction; an individual backup is at most 8 MiB. Unix directories
and files must be owner-only (0700 and 0600); Windows needs an owner-only DACL.
Reject squatted directories, links and reparse points. Failure to create a private
backup refuses the write. Logging records counts and fixed reasons only, never
paths or contents. Backup retention is independent of UI file changes.

Errors use the usual `code` and `message`, with optional `size` for `413 too_large`
and `current_revision` for conflicts. Additional `ErrorCode` values are
`too_large` (413) and `unsupported` (501). Bad paths, queries and bodies are 400;
unknown workstreams, locations and files are 404; links, escapes, hard-linked
write targets and `.git` writes are 403; I/O failures use fixed messages without
paths. Workstream lookup and file operations run off async threads.

Required implementation coverage: table tests for every lexical path rule;
symbolic-link and Windows-junction list/read/write refusal; deterministic
check/open and ancestor-swap tests; revision conflicts and exclusive creation;
hard-link refusal on both platforms; permission preservation; private backups,
squatting refusal and both retention caps. Shared mock/daemon conformance must
cover list/read/write, conflicts, agent refusal, traversal, absolute paths, 413
and 501. The daemon escape fixture must point only to a sibling temporary folder.
None of these files API tests have been implemented or run yet.

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
| `POST /v1/machines/{id}/scan` | → lines of `ScanFrame` (200) | Scans the machine's agent homes for onboarding. Device tokens only. See "Machine scan". |
| `GET /v1/machines/{id}/check` | → `MachineCheck` | What the machine has for running agents. The hub's owner only. See "Machine setup". |
| `GET /v1/machines/{id}/agents` | → `AgentAccount[]` | Each agent CLI's account, as its own status command reports it. The hub's owner only. |
| `POST /v1/machines/{id}/agents/{engine}/sign-in` | `StartSignIn`? → `SignIn` (201, or 200) | Runs the CLI's own login in a terminal. The hub's owner only. |
| `GET /v1/machines/{id}/agents/{engine}/sign-in` | → `SignIn` | That sign-in, and whether it still runs. The hub's owner only. |
| `DELETE /v1/machines/{id}/agents/{engine}/sign-in` | → 204 | Stops that sign-in and removes its terminal. The hub's owner only. |
| `GET /v1/members` | → `Member[]` | **agent** |
| `GET /v1/personas` | → `Persona[]` | |
| `POST /v1/personas` | `PersonaEdit` → `Persona` (201) | Device only; emits `persona_saved` and an owned agent `member_added` in one append. |
| `PUT /v1/personas/{id}` | `PersonaEdit` → `Persona` | Device only; unknown id 404; emits `persona_saved`. |
| `GET /v1/teams` | → `Team[]` | |
| `POST /v1/teams` | `TeamEdit` → `Team` (201) | Device only; emits `team_saved`. |
| `PUT /v1/teams/{id}` | `TeamEdit` → `Team` | Device only; unknown id 404; emits `team_saved`. |

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

`PersonaEdit`: `{ "name": String, "engine": Engine, "model"?: String,
"instructions"?: String, "permission_mode"?: PermissionMode }` (default `default`). Names are
trimmed, 1–80 code points, with no control characters; model is nonblank, control-free and at
most 200 code points when supplied; instructions are at most 32,000 code points (multiline).
Invalid fields answer 400 and append nothing. Ids and event authors are assigned by the hub.
Creating an agent recipe also creates an agent member owned by the caller, with that persona,
name and a friendly engine handle (`@claude`, `@codex`, `@opencode`, with a numeric suffix only on collision). This makes it selectable in teams without
changing `Persona` or `Team` fields. Editing a recipe updates its linked members' names in the
same append, only when every linked member is owned by the caller (otherwise 403; unlinked recipes are editable). Bypass permissions and models beginning with `-` are refused (400); saving a recipe launches nothing.

`TeamEdit`: `{ "name": String, "lead": MemberId, "members": MemberId[] }`. Name follows the
same rule. Members are existing people or agent members (including those linked to personas),
never persona ids. Unknown ids answer 400; duplicate ids are dropped and the lead is included.
At most 256 members; validation completes before any event is appended.

### Projects and workstreams

| Method and path | Body → response | Notes |
|---|---|---|
| `GET /v1/projects` | → `Project[]` | |
| `GET /v1/projects/{id}` | → `Project` | |
| `POST /v1/projects` | `NewProject` → `Project` (201) | See below. `409 conflict` if the key is in use. Emits `project_created`. |
| `GET /v1/workstreams?project=` | → `Workstream[]` | |
| `GET /v1/workstreams/{id}` | → `Workstream` | |
| `POST /v1/workstreams` | `NewWorkstream` → `Workstream` (201) | See below. `404 not_found` for an unknown project. Emits `workstream_created`. |
| `PATCH /v1/workstreams/{id}` | `{ "status"?, "health"?, "external"? }` → `Workstream` | Emits `workstream_changed` for status and health, `workstream_linked` for `external`: see "Linking a workstream upstream". |

`NewProject`, `NewWorkstream` and `NewTask` are Rust types in `crates/protocol/src/api.rs`.

`NewProject`: `{ "key": ProjectKey, "name": String, "lead"?: MemberId, "members"?: MemberId[],
"status"?: ProjectStatus, "start"?: Date, "due"?: Date, "root"?: Location, "first_workstream"?: String }`. The hub assigns `id`;
`external` starts empty.
- `key` is a `ProjectKey`: 2 to 10 characters, an uppercase ASCII letter, then uppercase letters or
  digits (`PAP`, `TL2`). Any other key is `400 invalid`; a key another project has is
  `409 conflict`. Task keys are `<key>-<n>`, starting at 1.
- `name` must not be blank.
- `lead` defaults to the caller. `members` defaults to the lead alone; the lead is always a member
  (put first when the list leaves it out), and duplicates are dropped. An unknown member is `400`.
- `status` defaults to `in_progress`.
- Dates are `YYYY-MM-DD`, and `start` ≤ `due` when both are set (`400`).
- `root` names a known machine and a non-empty path (`400`). The creation dialog requires an
  absolute path for the selected platform (drive/UNC on Windows, `/` on Unix). The hub also
  rejects relative or incompatible paths for its local machine. Setup records its OS and
  architecture from the hub platform.
- `first_workstream`, when supplied, is a nonblank workstream name. Both objects are validated
  before one atomic append of `project_created` and `workstream_created`; its location is the
  project's root. This optional extension avoids a half-created project if a second HTTP request
  fails. Standalone workstreams use `POST /v1/workstreams` as before.

`NewWorkstream`: `{ "project": ProjectId, "name": String, "status"?: WorkstreamStatus,
"locations"?: Location[] }`. The hub assigns `id`; `health` starts `on_track` and `external` empty.
- An unknown `project` is `404 not_found`, although it is in the body.
- `name` must not be blank. `status` defaults to `active`. Each location names a known machine and
  a non-empty path (`400`).

**Linking a workstream upstream.** `PATCH /v1/workstreams/{id}` with `external: ExternalRef[]`
replaces the workstream's linked external items (an empty list unlinks them all). People only.
- At most 16 links, no two with the same `system` and `key`. Each `url`, when given, is an
  `https://` URL of at most 2 KiB with no user name or password. `key` is 1 to 300 characters with
  no control characters.
- **What a sync acts on.** A `github` key `owner/repo` names the whole repository and
  `owner/repo#milestone:<n>` one milestone; a `jira` key `DEMO` names the whole project and `DEMO-5`
  an epic. Owner and repository names use GitHub's characters (`A-Z a-z 0-9 - _ .`, never `.` or
  `..` alone); Jira keys are an uppercase letter, then uppercase letters, digits or `_`, then for an
  epic `-<n>`. A key of any other shape is kept as a plain link, which no sync acts on. Anything
  that breaks the rules above is `400 invalid`.
- It appends `workstream_linked` `{ "workstream", "external" }` with the full new list. A patch
  that changes nothing appends nothing. `status` and `health` may come in the same patch (then
  `workstream_changed` comes first, in the same append); a patch with none of the three is
  `400 invalid`.
- Linking is what a sync acts on: see "Integrations".

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
  - when the session first reports `working`, or reports a finished transcript turn (including
    a first read whose final state is already idle), the task moves to in progress (`task_moved`, mover
    `agent`, authored by the agent for its owner), if an agent may move it there;
  - when the agent reports the work done, by moving its task to review (`pitcrew report <task>
    --review`), the dispatch finishes as `succeeded` (`dispatch_finished`) in the same
    transaction as the move. A task a person already moved to review counts as that report: the
    agent's move answers `200` with the task, unchanged, and the dispatch succeeds. The back
    office moves a task still in progress to review when a dispatch succeeds
    (`dispatch_to_review`);
  - when the session ends without that report, including its terminal's CLI exiting without an
    end hook, the dispatch finishes as `canceled`, summary "The
    session ended without a report." (it stopped work), or as `failed`, summary "The session
    ended before its CLI started.", if the runner never reported the session;
  - when its CLI never started (the hub stopped between the dispatch and the start, or the CLI
    ended without writing a transcript), or a Codex or OpenCode CLI's transcript did not appear
    within 15 minutes of its start (past that, no transcript is matched to it by folder), the hub
    finishes the dispatch as `failed` and ends the session, at its next start or once it sees
    that. Before retiring an exited terminal, the runner scans and matches transcripts already
    written: exit before the watcher's first read does not prevent adoption. Active dispatches
    remain watched after adoption, until a report or terminal exit finishes them. A start still
    under way is never taken for one that did not start.
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

Manual linking is person-only (`403` for an agent). At least one of `workstream` or `task`
is required (`400` otherwise). A task alone derives its workstream; when both are given, the
task must belong to that workstream (`400` otherwise, including a task without a workstream).
Unknown body references are `400`; an unknown session in the path is `404`. A workstream-only
link clears the session's task. Every successful link emits `session_linked`, with basis `manual`.

Folder/branch linking uses this machine's workstream locations: the deepest containing folder
wins, a matching branch wins at equal depth, and a tie between workstreams links neither.
`dispatch`, `claimed`, `manual`, and `imported` are firm links: folder/branch inference and a
re-stated discovery without a firm link never replace them. Imported links preserve the
assignment chosen at import; a person may deliberately replace them with a manual link.
New workstreams trigger another pass over existing runner sessions.

`StartSession`: `{ "machine": MachineId, "engine": Engine, "cwd": String, "agent"?: MemberId,
"task"?: TaskId, "brief"?: String, "persona"?: PersonaId, "model"?: String,
"permission_mode"?: PermissionMode, "title"?: String, "workstream"?: WorkstreamId }`. A title is trimmed, 1–200 Unicode
characters, with no control characters. It overrides the transcript's title without changing it.
`persona`, `model` and `permission_mode` are launch options
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
- Without them, the hub also records a `starting` session before launching. Every successful
  start answers `202` with its terminal, even without a prompt or transcript yet. The transcript
  adopts that id later. This makes the terminal available for the first prompt. Person starts without an agent or task
  are not subject to the 15-minute named-start deadline; a running terminal keeps them alive,
  and multiple such starts in one folder retain the existing best-effort folder matching.
- CLI availability, permission modes, bypass policy and argument constraints are checked before
  recording a start (400 with a plain reason). An ambiguous named start in the same folder is
  409 `conflict`. A runtime failure after validation can still end a recorded start.
- An explicit `workstream` links the session by hand before launching. Without one, a workstream
  whose location contains the resolved folder is inferred at start. Firm links survive adoption.

`GET /v1/machines/{id}/session-options` (person-only) returns
`SessionOptions`: `{ "platform": "windows" | "unix", "engines":
[{ "engine": Engine, "permission_modes": PermissionMode[], "first_prompt_forbidden": String[] }] }`. Only executable CLIs
on that runner's PATH are listed; none are run to detect availability. Modes reflect launch
support and the runner's bypass policy (Codex has no plan mode; OpenCode uses its settings).
Unknown machines are `404`; no runner, no terminal runtime or an unreachable machine is `503`.
Availability is advisory: a CLI removed after the check can still fail at launch.
`first_prompt_forbidden` lists characters that cannot be passed through an installed Windows
batch wrapper (control characters and `" % ! ^ & | < > ( )`); it is empty for native programs and
Unix CLIs. Such a prompt is refused before recording, with 400 and advice to enter it in the
terminal after starting. The dialog checks these limits and selects the saved safety default
only when supported by the chosen engine.

Working and Starting from transcripts become Idle after **five minutes** without
transcript writes, accepted hooks or a known live terminal. **Waiting or an open tool call gets
60 minutes**: a quiet permission decision or a long tool run should not look abandoned after
five minutes, while abandoned waits still expire within an hour. An unavailable terminal list
is unknown evidence: expiry is skipped and that failure is not cached. A deleted transcript
emits Idle (unless terminal evidence is alive or unknown), including after restart.
The runner checks this during its
existing safety sweeps (normally 30 seconds; 120 seconds on polled network homes). Old transcripts
are normalized before discovery, including after restart. Expiry clears the status line, emits
`session_state_changed` and never edits a transcript. A later write/hook can resume activity;
ended and unreachable states are preserved. Terminal/process evidence is obtained from the
runner's bounded runtime calls, never by guessing a process from its name.

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

### Read cursors

`GET /v1/me/cursors` returns `ReadCursor[]` for the token's person, sorted by scope.
`PUT /v1/me/cursors/{scope}` takes `{ "rev": u64 }` and returns a `ReadCursor`
(`{ "scope": String, "rev": u64 }`). Both routes are person-only: agent tokens get 403,
including malformed PUT bodies. Cursors belong to a person across all their devices.

Scopes are `workspace`, `project:<ProjectId>` or `workstream:<WorkstreamId>` (bare ULIDs).
Malformed scopes are 400; unknown projects/workstreams are 404. An absent cursor means 0.
Revisions ahead of the hub's current log are 400. A revision equal to or below the stored
one returns the current cursor without appending. Moving forward appends `cursor_moved`
with `{ "scope", "rev" }`, authored by the person; its projection keeps the maximum revision
per author and scope. Clients refetch cursors on this stream event. Cursor events are read
metadata, private to their person: never in activity or recap inputs, and never in
other people's streams (including replay). Only that person's device tokens receive
`cursor_moved`; agent tokens never receive it. The UI also excludes it defensively.

Activity and recap routes keep their existing paging contract. Clients fetch their cursor,
compare activity revisions to it, and mark revisions greater than it new. Home counts the
new items in its loaded window (and indicates when older pages remain). Mark all as read
advances to the newest activity revision actually shown. Project and workstream views
advance their own scope after a one-second dwell, to the newest activity revision loaded
when that visit began; arriving live events remain new until another visit.

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

**Activity paging** (`GET /v1/events`, also available as `GET /v1/activity`, response type
`EventsPage`): events oldest first within the page, the newest page when `before` is absent. `before` is an exclusive revision. `from_rev`
and `to_rev` are the revisions of the first and last returned events. `revisions: u64[]`
contains each returned event's actual revision in the same order; revisions need not be
contiguous, even without filters. Cursor writes are skipped before counting the limit,
so an unfiltered page holds up to its limit of real events. Pass `from_rev` as `before` for
the previous page. Default limit 100, max 500;
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
    `brief_proposed` and `safety_changed` are not activity: they change no recap.
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

### Machine scan

| Method and path | Body → response | Notes |
|---|---|---|
| `POST /v1/machines/{id}/scan` | none → lines of `ScanFrame` (200) | Device tokens only. See below. |

Onboarding's scan step: what agent sessions a machine has, and which projects and workstreams they
suggest. The types are in `crates/protocol/src/scan.rs`.

- **What it reads.** A read-only, bounded walk of the machine's agent homes (`~/.claude`,
  `~/.codex`, OpenCode's data folder, or where `CLAUDE_CONFIG_DIR`, `CODEX_HOME` and
  `XDG_DATA_HOME` point). On the daemon these are the runner's homes: `--homes` when given, none
  with `--demo` alone. It reads a prefix of each transcript (one indexed row of an OpenCode
  store), never a whole transcript, and never prompt text. It does not import sessions. During
  setup, every successful scan also ensures one dispatchable, caller-owned agent per detected engine
  (`counts.by_engine` with a positive count), before the `done` frame. Missing recipes and members
  are appended together as `persona_saved`/`member_added`, using default permissions. A person who
  already owns an agent with that engine's persona gets no duplicate; repeat/concurrent scans are
  idempotent. `@office` (no persona) never satisfies this requirement. Empty, failed or canceled
  scans create no agents; partial scans provision only engines actually detected. A provisioning
  failure returns an `error` frame and appends none of the new agents.
- **Which machine.** Only the hub's own (its first `local` machine). An unknown or malformed id
  is `404`. Another machine of the workspace is `409 conflict`: scanning it is not supported yet.
  A hub that reads no agent homes (the daemon with `--no-runner`) is `409` too. The body is
  ignored; send none.
- **One scan at a time per machine.** A scan holds its machine from the moment it is accepted
  until its walk ends; a second one meanwhile is `409 conflict`. A client that closes the answer
  early cancels further work between homes and files. A ten-minute budget also stops further
  work and returns the counts collected so far with `partial: true`. An in-flight filesystem
  operation must finish before cancellation takes effect and the machine can be scanned again.
- **The answer** is `200` with `Content-Type: application/x-ndjson`: one `ScanFrame` JSON object
  per line, written as the walk goes. Read it as a stream for live progress, or whole.
  - `{"type":"progress","scanned":0}` at once. Then `{"type":"progress","scanned":N,"total":M}`
    (with `"path"`, a transcript just read, when there is one) at most every 100 ms; the last
    progress frame has `scanned` equal to `total` for a complete scan; a partial scan may have less.
  - Then exactly one last frame: `{"type":"done","report":ScanReport}`, or, if the scan failed
    after the answer began, `{"type":"error","code":ErrorCode,"message":String}`.
  - A client that reads slowly may miss progress frames. Sending the last progress frame or
    final frame waits at most 30 seconds; on timeout the stream closes and releases its claim.
- `ScanReport`: `{ "counts": ScanCounts, "suggestions": Suggestion[], "unreadable": u64, "partial"?: bool }`.
  - `ScanCounts`: `{ "sessions", "subagent_sessions", "by_engine": [{ "engine", "count" }],
    "by_home": [{ "engine", "home", "count" }], "by_folder": [{ "path", "count" }],
    "by_month": [{ "month", "count" }], "first_activity"?, "last_activity"? }`. The `by_` lists
    count ordinary sessions; sub-agent sessions are counted once, in `subagent_sessions`.
    Sessions without a working directory are omitted from `by_folder`; sessions without a
    start time are omitted from `by_month`, so either list may sum to less than `sessions`.
    `by_folder` is busiest first, `by_month` (`YYYY-MM`, UTC) most recent first.
  - `Suggestion`, a suggested project: `{ "id", "name", "path", "is_git", "session_count",
    "recent_30d", "recent_90d", "workstreams": WorkstreamSuggestion[] }`. `path` is a repository
    root (the nearest folder with a `.git`), or a folder shared by several sessions' folders that
    have none; never the person's home, an agent home or a system folder. `id` is the path, stable
    across scans. Most recently active first: sessions in the last 30 days, then 90, then all.
  - `WorkstreamSuggestion`: `{ "id", "name", "branch"?, "session_count", "recent_30d",
    "recent_90d" }`. Either a first-level sub-folder of the project with sessions in it (no
    `branch`; `id` is the folder's own path, which is also where it is), or a branch other than
    `main`, `master`, `trunk`, `develop` and `HEAD` (with `branch`; `id` is
    `<project path>#<branch>`, and it is the project's `path` on that branch).
  - `partial`, when true, means cancellation or the budget stopped the scan early. Absent means
    false, for compatibility with older servers.
  - `unreadable` counts homes, folders and transcripts skipped because they could not be read;
    the rest of the scan still ran.
  - Paths are the machine's own, as its CLIs wrote them. A session's folder and branch are the ones
    it started with.
- **Privacy.** The report names the person's folders and branches. It goes only to the device
  token that asked, and is kept nowhere; an agent token gets `403`.
- **Creating from a scan** is `POST /v1/projects` (`root` at the suggestion's `path`) and
  `POST /v1/workstreams` (one location per suggested workstream, as above).
- **Why not `/v1/stream`.** The stream is the workspace's shared feed: every device connected
  receives every frame, and a scan's paths are one person's. The answer also keeps the progress,
  the result and its failure together, and closing it is how a client stops listening.
- **The mock** answers with a fixed synthetic report (folders under `/home/sam/`), after about a
  second of progress, by the same rules: the hub's own machine only, `409` while one runs.

### Machine setup

| Method and path | Body → response | Notes |
|---|---|---|
| `GET /v1/machines/{id}/check?row=` | → `MachineCheck` | `row` (optional) checks one row again. |
| `GET /v1/machines/{id}/agents` | → `AgentAccount[]` | Claude Code, Codex, OpenCode, in that order. |
| `POST /v1/machines/{id}/agents/{engine}/sign-in` | `StartSignIn` or none → `SignIn` | `201` when it starts one, `200` with the one still running. |
| `GET /v1/machines/{id}/agents/{engine}/sign-in` | → `SignIn` | `404` when there is none. |
| `DELETE /v1/machines/{id}/agents/{engine}/sign-in` | → `204` | Stops it, running or ended, and removes its terminal; `404` when there is none. |

Onboarding's machine steps, and the machine-setup wizard's: check what a machine has for running
agents, and sign in to each agent CLI with the CLI's own login. The types are in
`crates/protocol/src/machine_setup.rs`.

- **Who.** All five are for **the hub's owner only**: the member who set the hub up (its first
  person, `POST /v1/setup`'s). An agent token gets `403`, and so does any other member's device
  token, on every route and before anything else is looked at (the machine, the engine, the
  body): on a shared hub, another person cannot sign the owner's machine in to their own account,
  open the owner's sign-in, or read the owner's accounts. Before setup (no person yet) they are
  `409`.

- **Which machine.** Only the hub's own (its first `local` machine), as for the scan. An unknown or
  malformed id is `404`; another machine of the workspace is `409 conflict`: a remote machine is
  checked through its own hub (a remote workspace's, through the desktop gateway), or over SSH
  while it is being connected (desktop-gateway.md, `RemoteProbe.check`). `engine` is `claude`,
  `codex` or `opencode`; any other is `404`.
- **The check.** `MachineCheck`: `{ "rows": MachineCheckRow[] }`, in this order: `cli_claude`,
  `cli_codex`, `cli_opencode`, `tmux` (not on Windows, where PitCrew's terminals never use it),
  `git`, `gh`, `disk`, and `slurm` only where `sbatch` is on the machine's `PATH`. (`helper` is a
  row only of a check made over SSH before PitCrew is installed.)
  - `MachineCheckRow`: `{ "id", "status": "ok" | "warn" | "missing", "detail", "version"?,
    "fix"? }`. `detail` is one line for people: the tool's version line, the free space, or what
    is wrong. `version` is the tool's first line of `--version` (`-V` for tmux), cleaned of
    escapes and control characters, at most 120 characters.
  - A tool is looked for on the `PATH` of the hub (absolute entries only) and asked only its
    version, with no input, for at most 15 seconds; one that is not there is `missing`, one that
    fails or does not answer is `warn` with why. tmux older than 3.2 is `warn`. `disk` is the free
    space of the filesystem that holds the hub's state: `warn` under 5 GB. `slurm` is `warn` when
    `squeue` or `scancel` is missing.
  - **`fix`** says what PitCrew can do, and is absent on an `ok` row: `install_page` (the client
    opens that tool's install page, from **its own** table of pages by the row's `id`; no URL
    comes from the machine) or `install_helper` (install PitCrew's helper: the connect wizard's
    next steps). **Nothing installs a system package**, and no route runs a fix: a fix is the
    client's to show.
  - `?row=<id>` answers that row alone (none, where it does not apply); an unknown `row` is `400`.
- **Accounts.** `AgentAccount`: `{ "engine", "installed", "signed_in"?, "account"?, "detail"? }`,
  from each CLI's **own status command**, never from its files: `claude auth status`, `codex
  login status`, `opencode auth list`, each for at most 20 seconds.
  - `signed_in` is what the CLI said; absent when it could not tell (not installed, a CLI too old
    to have the command, a timeout, output not understood), with why in `detail`.
  - `account` is a label: the e-mail address Claude Code prints, `ChatGPT` or `API key` for Codex
    (never the rest of its line, which shows part of a key), the providers OpenCode lists. At most
    120 characters, and never anything that looks like a key or a token.
- **Sign-in.** `POST …/sign-in` runs the CLI's own login in a terminal on the machine, in the
  person's home folder, with nothing added to its environment: `claude auth login`, `codex login`,
  `opencode auth login`. `StartSignIn` is `{ "method"?: "browser" | "device_code" }`;
  `device_code` is for a machine the browser cannot reach back to, and only Codex has it (`codex
  login --device-auth`; `400` for the others). Unknown fields are `400`.
  - The answer is `SignIn`: `{ "engine", "terminal", "command": String[], "running", "started" }`.
    `terminal` is an id for the **terminals route** (`GET /v1/sessions/{terminal}/terminal`, see
    "Terminals"), the only route that knows it: it is not a session, appends no event, and `GET
    /v1/sessions/{terminal}` is `404`. The person drives the login there.
  - **The terminal opens only for the member who started the sign-in** (the hub's owner): any
    other member's device token gets `403` from the terminals route.
  - **Only a CLI that answers its status command.** The login starts only once the CLI's own
    status command (as for the accounts) has said whether it is signed in; otherwise `409`
    ("Update Claude Code first: …"). An older Claude Code without `auth` would read `auth login` as
    a prompt and start an agent instead.
  - **One per CLI at a time:** asking while one runs answers that one (`200`); two asks at once
    get the same one. An ended one is replaced by the next (`201`).
  - Once its login ends (`running: false`), the terminal stays readable for 5 minutes, then it is
    removed with its output; a login still running after 30 minutes is stopped. `DELETE
    …/sign-in` stops it at once (the client leaves the sign-in, or skips it). A hub that stops
    stops its sign-ins, and one that starts removes any sign-in terminal an earlier run left
    (their ids are kept in its state directory; a session's terminal is never taken for one).
  - `409` when the CLI is not installed, or does not answer its status command; `503` when the
    machine has no terminal runtime (tmux 3.2 or newer, or pitcrew-ptyd), or it does not answer.
  - **PitCrew never reads the login.** The terminal relays the CLI's screen and the person's keys,
    which nothing parses, logs or keeps; what the login stores is the CLI's, in its own files.
    After it ends, `GET …/agents` asks the CLI again.
- **The mock** answers a fixed synthetic check (Claude Code, Codex, tmux and git there; OpenCode and
  gh missing; no SLURM) and accounts (Codex signed in, Claude Code not, OpenCode not installed).
  Its sign-in terminal shows a canned login and ends when Enter is pressed in it, or by itself
  after about two seconds; Claude Code then reports `sam@example.com`. Its owner is the workspace's
  first person (`dev-device-token`'s, `@sam`); `dev-second-device-token` is another person. Same
  rules otherwise.

### Session import (device tokens only)

- `POST /v1/import/dry-run` accepts `ImportFilter` and returns `{ "count": N }`.
- `PUT /v1/import` accepts the same filter, stores it durably, and returns `{ "imported": N }`.
- `GET /v1/import` returns `{ "filter": ImportFilter, "committed_at": <milliseconds or null> }`.
  Settings may change the choice with the same PUT; no re-scan is required.
- `ImportFilter` has `mode: "all" | "filtered" | "none"`, optional `since` (`YYYY-MM-DD`,
  inclusive midnight UTC), `engines` and `folders`. In filtered mode, dimensions combine with
  AND; engines and folders within a dimension combine with OR. Missing or empty arrays impose
  no restriction. Folders match the exact working directory or a descendant at a separator
  boundary (both slash spellings accepted, case preserved). Invalid dates or empty folder names
  return 400. All/none ignore optional restrictions.
- All includes every indexed session. None means start fresh: sessions with `started` strictly
  after the commit time are included. Its dry run evaluates a prospective boundary at request time (normally zero). PUT counts at its commit boundary.
  Recommitting none establishes a new boundary. Before the first commit the default is all.
- Counts include sub-agent sessions and describe indexed sessions, not a fresh filesystem scan.
  A dry run and commit agree if no session was indexed between the requests. Later sessions obey
  the stored rules automatically. Excluding never deletes events or transcripts, moves or copies
  files, or stops the runner reading them. Widening the filter restores history immediately.
- Excluded sessions are absent from session lists and session detail/transcript/terminal reads
  (404), activity pages, recap blocks and day paragraphs, and replay/live stream events about
  those sessions. Non-session work remains visible. Cursor privacy still applies. Hidden event
  revisions are skipped without renumbering the log or ending pagination early. Clients refetch
  session lists, activity and recaps after committing a choice.
- All three routes reject agent tokens with 403, before reading a body.

### Integrations: GitHub and Jira

A person connects GitHub repositories or Jira projects to the workspace, links workstreams to
upstream scopes ("Linking a workstream upstream"), and the hub keeps tasks in step with upstream.
**A sync only reads upstream**: no route here writes to GitHub or Jira, and every write goes
through a person's approval first ("Outward writes", below). Every route is **device
tokens only** (an agent token gets `403`, before anything else is checked). Types are in
`crates/protocol/src/integrations.rs`.

| Method and path | Body → response | Notes |
|---|---|---|
| `GET /v1/integrations` | → `Integration[]` | Oldest first. |
| `POST /v1/integrations` | `NewIntegration` → `Integration` (201) | See below. |
| `GET /v1/integrations/{id}` | → `Integration` | |
| `DELETE /v1/integrations/{id}` | → 204 | Forgets the connection, its stored credential and its sync state. Links on workstreams stay, as plain links. A sync of it under way applies nothing more and keeps no state. |
| `POST /v1/integrations/{id}/test` | → `IntegrationCheck` | Reads upstream once with the credential (see below). Changes nothing. |
| `POST /v1/integrations/{id}/sync` | → `Integration` (202) | Syncs now, in the background; `status.running` is `true` until it ends. |
| `PUT /v1/integrations/{id}/credential` | `{ "secret": String }` → 204 | Stores the secret. See "Credentials". |

`NewIntegration`: `{ "name": String, "settings": IntegrationSettings, "credential":
"gh_cli" | "stored", "interval_minutes"?: u32 }`.
- `name` is 1–80 characters after trimming (stored trimmed), no control characters.
- `settings` is tagged by `kind`:
  - `{ "kind": "github", "repos": String[], "api_base"?: String }`: 1–50 distinct `owner/repo`
    (GitHub's characters, as for links). `api_base` is a GitHub Enterprise Server API root
    (`https://ghe.example.com/api/v3`); absent means `https://api.github.com`, and that root given
    explicitly is kept as absent.
  - `{ "kind": "jira", "deployment": "cloud" | "data_center", "site": String, "projects":
    String[], "email"?: String, "epic_link_field"?: String }`: `site` is the Jira root
    (`https://jira.example.com`, a path allowed for Data Center); 1–50 distinct project keys;
    `email` (1–254 characters with an `@`) is required for `cloud` and refused for
    `data_center`; `epic_link_field` is `customfield_<digits>` (Data Center's epic link).
  - Every URL is `https://`, at most 2 KiB, with no user name, password, query or fragment.
- `credential`: `gh_cli` (GitHub only) reads `gh auth token --hostname <host>` on the hub's
  machine at each sync and keeps nothing. The host is always named (`github.com`, or the
  Enterprise server's), and `GH_HOST` is cleared for `gh`, so its default host never decides
  which token is sent where. `stored` waits for a secret (`PUT …/credential`).
- `interval_minutes` is 5–1440; default 15.
- A repository or Jira project already in another integration is `409 conflict`, on any host:
  workstream links and task sources name a repository or an issue key without its host, so the
  same one on two hosts (github.com and an Enterprise server, or two Jira sites) would move each
  other's tasks. Anything else malformed is `400 invalid`.
- Each integration acts through a member of its own, owned by the person who added it: `@sync` (an
  agent of that person, named "Tracker sync"), or `@tracker-sync` when `@sync` is another person's,
  added with `member_added` on that person's first `POST`. When both handles are other people's,
  the `POST` is `409 conflict`. Everything a sync changes is authored by its integration's member.

`Integration`: `{ "id", "name", "settings", "credential": { "source": "gh_cli" | "stored",
"stored": bool }, "interval_minutes", "added_by": MemberId, "added_at": ms, "status":
SyncStatus, "links": IntegrationLink[] }`.
- `credential.stored` says whether a secret is kept (always `false` for `gh_cli`). **No route ever
  returns a credential.**
- `SyncStatus`: `{ "running": bool, "last_attempt_at"?, "last_success_at"?, "next_at"?,
  "rate_limited_until"?, "problems": SyncProblem[], "last_run"?: SyncCounts }` (times in ms).
  `last_success_at` is the end of the last sync that read every scope and applied what it found
  without a problem; that is the "last sync" people see. `problems` are the last sync's, each
  `{ "scope": String, "message": String }` (`scope` is a repository, a Jira project, or `""` for
  the whole integration), never holding a credential. `SyncCounts`: `{ "changes", "applied",
  "conflicts", "skipped", "malformed" }`.
- `IntegrationLink`: `{ "workstream": WorkstreamId, "scope": ExternalRef, "title"?: String }`: each
  workstream link this integration syncs (its repositories, their milestones, its Jira projects and
  their epics), with the upstream title once a sync has seen it.

`IntegrationCheck` (`POST …/test`): `{ "ok": bool, "at": ms, "checks": [{ "scope", "ok",
"message" }], "warnings": String[] }`. One check per repository (`GET /repos/{owner}/{repo}`) or
per Jira project (`GET /myself`, then `GET /project/{key}`), and one with scope `""` for the
credential itself. `warnings` says when the credential can do more than read (a GitHub
credential with push or admin rights on a repository, or a classic token's broad scopes): a
fine-grained, read-only token for these repositories is safer. `ok` is `true` when every check
passed.

**Credentials.**
- `PUT /v1/integrations/{id}/credential`: `secret` is 1–4096 characters, no whitespace or control
  characters. `409 conflict` for a `gh_cli` integration. It replaces any secret stored before. The
  desktop sends it through the gateway's own command (`gateway_integration_credential`, see
  `desktop-gateway.md`), never through `gateway_request`.
- The hub keeps a secret in a private file (0600 in a 0700 folder; an owner-only DACL on Windows)
  of its state directory, never in the event log, and never logs it.
- Jira Cloud authenticates with `email` and the secret (an API token); Data Center with the secret
  as a personal access token; GitHub with the secret or `gh auth token` as a bearer token.

**What a sync does.** On a timer (`interval_minutes`, the first one soon after the hub starts or
the integration is added) and on `POST …/sync`, one integration at a time:
- It reads each scope incrementally (`ETag`s and `since` on GitHub, an `updated` cursor in JQL on
  Jira), and stops at a rate limit until it lifts (`rate_limited_until`). Only what changed
  upstream since the last read acts: a field, a move or a shipped workstream a person changed in
  the hub stays as they left it until upstream changes again.
- It reaches GitHub and Jira directly, or through the `http://` proxy `HTTPS_PROXY` names (not for
  the hosts `NO_PROXY` names), with `CONNECT`: TLS stays end to end, so the proxy never sees a
  credential.
- **Issues become tasks only in a linked scope.** An open issue whose milestone (GitHub) or epic
  (Jira) a workstream links becomes a task in that workstream; otherwise one whose repository or
  Jira project a workstream links. So does an open issue a later sync finds moved into a milestone
  or under an epic that routes to a workstream this way. Issues in no linked scope, and issues
  already closed when first seen, are skipped (`skipped`). The task's `source` is the issue, its
  status `todo`.
- **Field owners** (the tables are in "Outward writes"). Title, description and labels belong to
  upstream: an upstream change overwrites them (`task_updated`). The assignee belongs to the hub. A
  change of milestone or epic moves the task to the workstream that links the new one (in the same
  project); otherwise it stays.
- **Moves follow `can_move(.., sync)`.** An upstream close moves the task to `done`, a reopen moves
  a done task to `todo` (`task_moved`, mover `sync`). In-progress work is never touched: a move the
  rules refuse becomes an ask instead (a **conflict**, below).
- A merged pull request that closes a tracked issue is noted on its task (`comment_posted`, with
  the pull request's link).
- **Milestones and epics.** A closed milestone or epic moves the workstreams that link it to
  `shipped` (`workstream_changed`), unless one of their tasks is in progress (a conflict). The
  hub owns the workstream's name; the upstream title is shown on the link.
- **Conflicts become asks**: `ask_raised`, kind `decision`, from `@sync` to the person who added
  the integration, with the task when there is one. Nothing is changed; the person decides. The
  same open conflict is not raised twice.
- Upstream text is untrusted: titles, bodies and labels are capped and stripped of hidden
  characters, labels are cut to the hub's rules (1–64 characters, at most 32), and links are kept
  only on the tracker's own host.
- Changing a scope's links makes the next sync read that scope's issues again from the start, so
  issues that were out of scope before become tasks.
- **Who sees it.** What a sync appends (`member_added`, `task_created`, `task_updated`,
  `task_moved`, `workstream_changed`, `comment_posted`, `ask_raised`), and `workstream_linked`,
  reaches `/v1/events`, `/v1/activity` and `/v1/stream` through the same visibility rule as every
  event ("Session import"). None of it names a session, so it is non-session work: an import
  choice never hides it. Those routes, like these, are device tokens only.

**The mock** answers every route over the recorded fixtures in `apps/mock-hub/fixtures/`
(`example-org/demo-repo` on GitHub, project `DEMO` on `https://jira.example.com`), or the folder
`startServer({ integrationFixtures })` names, syncs at once on `POST …/sync`, and keeps credentials
in memory only. The daemon reads them the same way when started with the hidden
`--integration-fixtures <dir>` (tests only; it then never reaches the network). Both read the
folder again at each sync, so a test changes what upstream says by adding a file whose name sorts
first.

### Outward writes: every one approved first

PitCrew can change GitHub and Jira (create an issue from a task, comment, close or reopen, change
the title, description or labels, set a milestone or epic), but **nothing is sent upstream until a
person approves it**. A hub change that implies a write raises an ask of kind `approval` that shows
exactly what will be sent; the hub sends it only after the person answers **Send**, and records
the result as an event. Every route here is **device tokens only** (an agent token gets `403`
before anything else is checked). Types are in `crates/protocol/src/writes.rs`.

| Method and path | Body → response | Notes |
|---|---|---|
| `GET /v1/writes?task=&state=` | → `UpstreamWrite[]` | Oldest first; `state` may repeat. |
| `GET /v1/writes/{id}` | → `UpstreamWrite` | `id` is the approval ask's id. |
| `POST /v1/writes` | `NewWrite` → `UpstreamWrite` (201) | A person asks for a write: see below. |
| `POST /v1/writes/{id}/retry` | → `UpstreamWrite` (202) | Asks to send a `failed` write again: see "Retrying". |

**Field owners, both ways.** Each field has one owner. A sync applies upstream's changes to the
fields upstream owns ("What a sync does"); a person's change in PitCrew to a field upstream owns is
a conflict, raised as an approval to send it. The hub's own fields are never sent.

GitHub issue ↔ task:

| Field | Owner | Upstream → PitCrew (a sync) | PitCrew → upstream (after approval) |
|---|---|---|---|
| `title` | GitHub | overwrites the task's title | a person's change: `update`, only when the hub holds upstream's title exactly ("Only what the hub holds exactly") |
| `body` | GitHub | overwrites the task's description | a person's change: `update`, only when the hub holds upstream's body exactly |
| `labels` | GitHub | overwrite the task's labels | a person's change: `update`, as the labels added and removed; every other label upstream is kept |
| `milestone` | GitHub | moves the task to the workstream that links the new milestone (same project) | moving the task to a workstream that links another milestone of its repository: `update` |
| `state` | both, through rules | a close moves the task to `done`, a reopen a done task to `todo`, when `can_move(.., sync)` allows | a move into `done` or `canceled` closes the issue (`close`, reason `completed` or `not_planned`); a move out of them reopens it (`reopen`) |
| `assignees` | PitCrew | never read into the task | never sent |

Jira issue ↔ task: the same, with `summary` for the title, `description` for the body, the
**epic** for the milestone (Cloud's `parent`; Data Center's `epic_link_field`, so Data Center gets
no epic writes without one), and the status category for the state (`close` is a transition into
the Done category, `reopen` one into To Do; the first such transition the issue's workflow offers).
The assignee belongs to PitCrew.

**What raises an approval.**
- **Implied by a change** in the event log, authored by anyone but a sync (any integration's own
  sync member: a sync's changes come from upstream and are never sent back), on a task whose
  `source` is an issue of a repository or Jira project an integration syncs:
  - `task_moved` across the open/closed line (`done` and `canceled` are closed): `close` or `reopen`;
  - `task_updated` with `title`, `description` or `labels`, or a `workstream` that links another
    milestone or epic of the issue's repository or project: one `update` with those fields, for an
    issue a sync has read (an `update` is checked against upstream's values as last read; a change
    to an issue no sync has read yet, such as one PitCrew just created, proposes nothing).

  Nothing is raised when upstream already has the value (as the last sync read it), and one change
  raises one approval, never two (it names the change as `cause`). Creating a task never creates an
  issue by itself.
- **Only what the hub holds exactly is sent back.** A sync keeps upstream's text in the form the
  hub can hold: hidden characters stripped, titles and bodies cut to their caps, Jira Cloud's rich
  text turned into plain lines. Writing that copy back would replace what PitCrew never held, so:
  - the **title** and the **description** are proposed only when the last read was lossless
    (`IssueSnapshot::title_lossless` and `body_lossless` in the sync crates): what the hub holds
    equals what upstream sent, and on Jira Cloud the description is plain paragraphs of
    unformatted text, the form PitCrew writes. Otherwise that field is left out (the ask says so
    when other fields are sent), nothing is raised for it alone, and the hub keeps its own value
    until upstream next changes it;
  - **labels** are sent as a change, `add_labels` and `remove_labels`: the labels the person added
    and removed, against upstream's as last read and as the hub holds them (at most 32, each cut to
    64 characters). Labels upstream has that the hub does not hold are never touched.
- **Asked for by a person**, `POST /v1/writes` with `NewWrite`: `{ "task": TaskId, "operation":
  "create_issue" | "comment", "text"?: String }`.
  - `create_issue`: the task must not mirror an issue yet (`409`), and its workstream must link a
    scope an integration syncs (`400` otherwise): the first such link in the workstream's list
    names the repository or project, and the milestone or epic when it is one. It sends the task's
    title, description and labels.
  - `comment`: the task must mirror an issue of an integration (`409` otherwise); `text` is 1 to
    65,536 characters, with no control characters but line breaks and tabs.
  - `operation` anything else, an unknown task, or a missing `text` for a comment is `400`.

**The approval ask.** `ask_raised` (kind `approval`, from the integration's own sync member, to the
person who added the integration, with the task) and `write_proposed` (`{ "write": WriteProposal }`) are appended
together. The ask's title names the tracker, the operation and the issue; its body lists every
field as `before → after`; its options are `["Send", "Don't send"]`. `UpstreamWrite.proposal` holds
the same, structured:

`WriteProposal`: `{ "ask": AskId, "integration": IntegrationId, "system": "github" | "jira",
"scope": String, "target"?: ExternalRef, "task"?: TaskId, "operation": WriteOperation, "before":
WriteFields, "after": WriteFields, "requested_by": MemberId, "cause"?: EventId }`.
- `scope` is the repository (`owner/repo`) or Jira project (`DEMO`); `target` the issue (absent for
  `create_issue`).
- `WriteOperation`: `create_issue`, `comment`, `update`, `close` or `reopen`.
- `WriteFields`: `{ "title"?, "body"?, "labels"?: String[], "add_labels"?: String[],
  "remove_labels"?: String[], "milestone"?: String, "epic"?: String, "state"?: "open" | "closed",
  "close_reason"?: "completed" | "not_planned", "comment"? }`. `after` is **exactly what is sent**,
  and only the fields being changed; `before` is upstream's value of each, as the last sync read it
  (absent when the hub has not read it). `labels` in `after` is a new issue's whole list
  (`create_issue`); an `update` sends `add_labels` and `remove_labels` instead, with upstream's
  labels as last read in `before.labels`. `milestone` is a link key (`owner/repo#milestone:2`),
  `epic` an issue key (`DEMO-5`).
- `requested_by` is whose change implied it, or who asked; `cause` the event that implied it.

**Answering, and what is sent.** `POST /v1/asks/{id}/answer` (a device token, the person the ask is
addressed to) with `{ "option": 0 }` approves; any other answer is a denial.
- **Approved:** the hub checks the write is still what the task says (each field of `after` still
  equals the task's, the move's status is still on the same side of the open/closed line, the
  integration still exists), then appends `write_started` (`{ "ask", "task"?, "attempt" }`).
- **Upstream as it is now.** For an `update`, `close` or `reopen` it then reads the issue (one
  `GET`: GitHub `…/issues/{n}`, Jira `…/issue/{key}?fields=…`) and compares it with `before`, field
  by field:
  - a field upstream already holds as `after` is not sent again;
  - a field upstream changed since it was read (it is neither `before` nor `after`, or the
    description now has formatting) means **nothing is sent**: `not_sent` ("Not sent: <issue>
    changed upstream since this was proposed (title). …"). The next sync brings upstream's change
    into the hub;
  - labels are taken one by one: a label to add that upstream has, or one to remove that it no
    longer has, is skipped, and a label to remove is removed under upstream's own spelling;
  - when nothing is left to send, the write is `sent` with no request.
- **What is sent:** what is left of `after`, with the integration's credential, and nothing else:
  GitHub one `PATCH …/issues/{n}` (title, body, milestone, state), then `POST …/issues/{n}/labels`
  with the labels to add, then one `DELETE …/issues/{n}/labels/{name}` per label to remove; Jira one
  `PUT …/issue/{key}` with `fields` and `update.labels` (`add` and `remove`), or a transition. Each
  request is sent once; a refusal stops the rest. Then `write_finished` (`{ "ask", "task"?,
  "result": WriteResult }`):
  - `{ "outcome": "sent", "created"?: ExternalRef, "url"?: String }`: `created` is the new issue
    (`create_issue`), and the task's `source` becomes it; `url` links what was written;
  - `{ "outcome": "failed", "message": String, "status"?: u16 }`: upstream refused it (its HTTP
    status, its message capped and stripped of hidden characters) or could not be reached.
  A write that is no longer what the task says, or whose integration is gone, is not sent:
  `{ "outcome": "not_sent", "reason": String }`.
- **Denied:** `write_finished` with `not_sent` ("Not sent: <person> chose not to."). Nothing reaches
  upstream, and the hub keeps its own value.
- Only an approval ask the hub raised with its `write_proposed` can send anything, and only when
  it comes from the write's own integration's sync member and a person answered it: an ask of kind
  `approval` raised through `POST /v1/asks` never does.

`UpstreamWrite`: `{ "proposal": WriteProposal, "state": WriteState, "attempts": u32, "proposed_at":
ms, "answered_at"?: ms, "answered_by"?: MemberId, "finished_at"?: ms, "result"?: WriteResult,
"retry_requested_by"?: MemberId }`. `retry_requested_by` is the person whose retry waits to be
sent (see "Retrying").
`WriteState`: `pending` (waiting for the person), `approved` and `denied` (answered, about to be
sent or recorded), `sending`, `sent`, `failed`, `not_sent`.

**Retrying, and sending at most once.** Each attempt is one `write_started`, followed by exactly one
`write_finished`.
- `POST /v1/writes/{id}/retry` asks to send a `failed` write again, the same `after`: it appends
  `write_retry_requested` (`{ "ask", "task"?, "by": MemberId }`, authored by that person) and
  answers `202` with the write. A second request while one waits appends nothing (`202` too).
  `409 conflict` in any other state (a sent write is never sent again); `403` for a person who may
  not answer its ask; `404` for an unknown id.
- A failed write starts again only with a `write_retry_requested` by a person that no
  `write_started` has used yet, so the log shows who asked for each attempt after the first.
- **Before a `create_issue` or a `comment` is sent again**, the hub looks upstream for the earlier
  attempt, since the first may have arrived though its answer was lost:
  - GitHub: `GET …/issues?state=all&sort=created&direction=desc&per_page=100&since=` for an issue
    with the same title and body, created since the write was approved (ten minutes' margin), or
    `GET …/issues/{n}/comments?since=&per_page=100` for a comment with the same text;
  - Jira: `GET …/search/jql` (Cloud) or `…/search` (Data Center) with `project = "KEY" AND reporter
    = currentUser() ORDER BY created DESC` for an issue with the same summary and description, or
    `GET …/issue/{key}/comment?orderBy=-created&maxResults=100` for a comment with the same text,
    created since.

  When it is there, the attempt is `sent` with it (a found issue becomes the task's `source`) and
  nothing is sent again; when the look-up fails, the attempt is `failed` and nothing is sent.
- A result the hub could not record (its database was busy) is kept in memory and recorded first
  at the next pass, before writes still `sending` are swept.
- A `write_started` with no `write_finished` (the hub stopped while sending) is finished as `failed`
  ("The hub stopped while sending; a retry first looks upstream for this attempt") when the hub
  starts again. It is never sent again by itself.
- Writes run one at a time with the integration's syncs, so a sync never reads an issue PitCrew is
  creating before the task links it.

**The mock** proposes, answers and records writes by the same rules, over the same fixtures: a
write is `sent` when `apps/mock-hub/fixtures/` holds an exchange for its method and URL with a 2xx
status, `failed` with that status otherwise, or with "no recorded fixture" when there is none. It
reads the issue before an `update`, `close` or `reopen` from its copy of upstream, and looks for an
earlier attempt before a retried create or comment in the same fixtures. A sent write changes the
mock's copy of upstream, so its next sync agrees. The daemon's `--integration-fixtures` answers
writes from the same files.

## Live updates: `GET /v1/stream?since=<rev>` (WebSocket, device tokens)

- Text frames, each one `StreamFrame` JSON.
- The first frame is `{"type":"hello","rev":N,"log":"<id>"}`. If `since` is given and older than
  `N`, the server then sends the missed events as `events` frames before live ones. A client that
  reconnects with its last `to_rev` receives exactly what it missed.
- **`log` identifies the hub's event log.** It is created with the store and never changes.
  Revisions only count within one log: if `log` differs from the one the client's cache came
  from, or `since` is newer than `N`, the client must drop its cached state and refetch.
- `events` frames carry contiguous `from_rev..=to_rev` and the events in order.
  Private cursor writes create gaps between frames: clients accept those gaps without resetting
  or renumbering events. Replay scans past hidden revisions, including metadata-only pages.
  No empty frame or cursor payload is sent for hidden writes. Small changes are batched
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

## Onboarding hooks and safety

All routes below are **device only** (agents receive 403). Hook routes act on the hub's own
machine (remote-hub onboarding is out of scope): an unknown machine is 404; another registered
machine is 501 `unsupported`. They never access CLI logins or log configuration contents.

- `POST /v1/machines/{id}/hooks/diff` → 200 `HooksDiff`: `{revision, files, engines}`.
  Each file is `{path, before: string|null, after: string}`; each engine is
  `{engine, status, detail}`. CLIs found on PATH or from their homes are planned, with the
  same installer and automatic Claude hook-form selection as `pitcrew hooks install`.
  Foreign Codex notify commands conflict; chaining is not enabled. No writes occur.
- `POST /v1/machines/{id}/hooks/install` with `{revision}` → 200 `{installed: boolean, skipped: string[]}`.
  The revision identifies a server-retained plan belonging to the requesting person, valid
  for ten minutes. No paths or replacement text are accepted from the request. All files are
  checked against the preview before applying; conflicting engines are skipped. A changed file, expired or
  unknown revision is 409 `conflict`. Repeating a successful apply is a no-op while every file
  still matches the installed result. Atomic writes and private timestamped backups use the
  CLI installer. Multi-file application is not transactional: an I/O failure may leave a
  partially installed plan; retry resumes unchanged/already-applied files, or request a new diff.
  At most 32 previews and 16 MiB of retained configuration text are held at once; older previews
  can be evicted. Config files larger than 1 MiB are refused for this API.
- `GET /v1/safety` → 200 `SafetySettings`.
- `PUT /v1/safety` with `SafetySettings` → 200 the saved settings. Shape:
  `{permission_mode: "default"|"plan"|"accept_edits"|"bypass_permissions",
  back_office_enabled: boolean, back_office_caps: {max_auto_accept_per_hour: integer 0..100}}`.
  Defaults are `default`, `false`, and 20. Saving `bypass_permissions` is refused with 400
  while the runner disallows it; the wizard shows this option disabled with the reason. Changes append `safety_changed {settings}`;
  identical saves append nothing. A projection rebuild and daemon restart retain the settings.

  New sessions without an explicit `permission_mode` use the saved workspace default.
  Saving safety settings immediately limits back-office task completion and automatic brief
  acceptance; other office actions retain their existing rules and caps. Before the first
  explicit safety save, existing hubs retain their per-task automatic acceptance policy.
  The hourly acceptance count is derived from the authored log and survives restarts. A capped
  task completion is refused; a brief is still proposed for a person to accept.

Safety uses `model::PermissionMode` on the wire. Saving `bypass_permissions` is
currently refused with 400 because the runner does not allow it. Session starts
and dispatches without a persona mode use the saved workspace default. Before
the first save, GET safety adds `saved: false`: legacy per-task acceptance policy
remains effective until an explicit policy is saved. Stored safety events accept
unknown fields for forward replay; PUT requests reject unknown fields.

Hook installation applies nonconflicting engine plans, returning
`{installed: boolean, skipped: string[]}`; `installed` is false when the preview
has no changes. Conflicting engines are excluded from preview files and skipped,
with their status shown to the person. CLI homes also establish engine presence
when a GUI process lacks CLI binaries on PATH. The desktop ships `pitcrew` beside
`pitcrewd`, and the Hooks step disables Install when there are no file changes.

### Directory write safeguards

`PUT /v1/personas/{id}` answers `403` unless every member linked to the persona is owned by
the caller; an unlinked persona is editable by any person. Refusals append no events and rename
no members. Both persona write routes reject `bypass_permissions` with the same message as
saving safety settings, and reject a trimmed model beginning with `-` (`400 invalid`).
New agents use `@claude`, `@codex`, or `@opencode`, adding `-2`, `-3`, … only when a handle is
already taken. Handles remain stable on edits. Dispatch requires an agent linked to an existing
persona; service actors such as `@office` and `@sync` cannot be dispatched (`400 invalid`).
Every successful machine scan provisions missing owned agents for the engines it detected;
repeated scans reuse existing owned, persona-linked agents.
