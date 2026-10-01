# pitcrew-hub-work

The hub's model of the work: projects, workstreams, tasks and subtasks, sessions and dispatches,
asks, comments, briefs, and the workspace's machines, members, personas and teams. Everything is
built from the event log (ADR-0004, ADR-0007).

**Owned by stream E** — see [docs/build/streams/E.md](../../docs/build/streams/E.md).

## Wiring

```rust
let store = Arc::new(Store::open_with(path, StoreOptions::default(), pitcrew_hub_work::projections())?);
let work = Arc::new(WorkService::new(Arc::clone(&store), workspace_id));
let parts = RouterParts::new()
    .agent(pitcrew_hub_work::agent_routes().layer(Extension(Arc::clone(&work))))
    .device(pitcrew_hub_work::device_routes().layer(Extension(work)));
```

To serve the demo workspace, open an empty store and call
`work.seed(&pitcrew_fixtures::demo_workspace()?)` (the service's workspace must be the demo's).

## Projections

| Name | Tables (migration) | Events |
|---|---|---|
| `work.directory` | `work_machines`, `work_members`, `work_personas`, `work_teams`, `work_team_members` (0200) | `machine_added`, `machine_liveness`, `member_added`, `persona_saved`, `team_saved` |
| `work.projects` | `work_projects`, `work_project_members`, `work_workstreams`, `work_locations` (0201) | `project_created`, `workstream_created`, `workstream_changed` |
| `work.tasks` | `work_tasks`, `work_subtasks`, `work_task_deps`, `work_task_labels` (0202) | `task_created`, `task_moved`, `task_assigned`, `subtasks_replaced` |
| `work.sessions` | `work_sessions`, `work_dispatches` (0203) | `session_*`, `turn_ended`, `tool_ran`, `file_edited`, `dispatch_started`, `dispatch_finished` |
| `work.asks` | `work_asks` (0204) | `ask_raised`, `ask_answered` |
| `work.comments` | `work_comments`, `work_comment_mentions` (0205) | `comment_posted` |
| `work.briefs` | `work_briefs`, `work_brief_proposals` (0206) | `brief_proposed`, `brief_accepted` |

Rules, so that applying events one append at a time and rebuilding give identical tables (tested
in `tests/rebuild.rs`):

- `apply` has no side effects (no appends, no clock) and uses the event's `at`, `author` and
  `on_behalf_of`, never a live caller;
- a projection reads and writes only its own tables; there are no foreign keys between
  projections, and joins happen in reads;
- an event about something unknown (a move of a task the hub never saw) changes nothing;
- a later event that re-states a row (`task_created`, `ask_raised`, `session_discovered` with a
  known id) updates it in place; lists keep the order rows were first created in (`rev`);
- bump a projection's `VERSION` with any change to its `apply` or its tables.

**Tasks.** Each `work_tasks` row keeps the whole task as the API returns it (`doc`, the
protocol's `Task` as JSON) next to the columns lists filter on (project, workstream, assignee,
status, key). A list is one indexed query, and `GET /v1/tasks` sends the stored documents as they
are. Subtasks, dependencies and labels are also indexed one per row, for queries by label, by
blocker or by subtask. Every change rewrites the document and those rows together; the tests check
they agree. Because documents are serialized protocol values, bump `work.tasks`' version when
`Task` changes shape.

**Briefs.** `brief_accepted` carries only `text` and `pinned`. The projection derives the rest:
`updated` is the event's time; the source is `back_office` when the back office applied it (an
agent's event) or when the text is exactly the target's latest proposal (a person accepted it
unchanged), and then the proposal's receipts carry over; otherwise it is `person`, with no
receipts. `next` is always empty until the event carries it (see "Contract gaps").

## Commands

`WorkService` validates each command against the tables, then appends its events in one
transaction, stamped from the `Caller` (`author` = the caller, `on_behalf_of` = an agent's owner).
Commands run one at a time; reads never wait for them.

| Command | Who | Refusals |
|---|---|---|
| `create_task` | person | 400 empty title, unknown project/workstream/assignee, workstream of another project, bad date |
| `move_task` | person; agent on its own task | 404; 403 agent on another's task; 409 when `can_move` says no |
| `assign_task` | person | 404; 400 unknown member |
| `replace_subtasks` | person (whole list); agent on its own task (only its own `agent_plan` lines, in place) | 404; 403; 400 empty text, repeated ids, `agent_plan` naming a non-agent |
| `post_comment` | person; agent on its own task | 404; 403; 400 empty text, unknown mention |
| `raise_ask` | anyone; an agent only about its own task and session | 400 unknown addressee, task or session; 403 |
| `answer_ask` | the addressee, or a person for their agents; decisions, approvals and reviews only by people | 404; 400; 403; 409 already answered |
| `put_brief` | person | 404 unknown target |
| `patch_workstream` | person | 404; 400 empty patch |
| `dispatch_working` | the runner link | moves the dispatched task to in progress as the agent, when the rules allow |
| `mirror_plan` | the runner link | replaces the agent's `agent_plan` lines of its own task from a `PlanUpdated` |
| `seed` | the daemon | imports a `DemoWorkspace` into an empty work model |

"Own task" means the agent is the assignee or holds an active (not ended) dispatch on it.

## Routes

Agent and device tokens (`agent_routes`): `GET /v1/me`, `GET /v1/members`,
`GET /v1/tasks?project=&workstream=&assignee=&status=`, `GET /v1/tasks/{id-or-key}`,
`POST /v1/tasks/{id}/move`, `PUT /v1/tasks/{id}/subtasks`, `POST /v1/tasks/{id}/comments`,
`GET /v1/asks?to=&state=`, `POST /v1/asks`, `POST /v1/asks/{id}/answer`.

Device tokens only (`device_routes`): `GET /v1/machines`, `GET /v1/personas`, `GET /v1/teams`,
`GET /v1/projects`, `GET /v1/projects/{id}`, `GET /v1/workstreams?project=`,
`GET|PATCH /v1/workstreams/{id}`, `POST /v1/tasks`, `POST /v1/tasks/{id}/assign`,
`GET /v1/briefs`, `PUT /v1/briefs/{project|workstream}/{id}`.

Errors are `ApiError` bodies: a malformed id or unknown thing in the path is `404`, in the body or
query `400`; bodies over 1 MiB are `400`.

## Tests

- `tests/seed.rs`: after `seed`, every list and item route equals the demo fixture structurally
  (briefs differ only by `next`). The ignored `seeded_routes_answer_like_the_running_mock_hub`
  compares against answers dumped from the running mock hub (`PITCREW_MOCK_DUMP`).
- `tests/rebuild.rs`: rebuilding every projection, building them on open, and applying one event
  per append all give identical tables.
- `tests/rules.rs`: the whole move-rules table through the routes, agent scope, who answers asks.
- `tests/routes.rs`, `tests/self_moving.rs`: the other commands and routes.

## Timings

`cargo test -p pitcrew-hub-work --release --test perf -- --ignored --nocapture` lists 10,000
tasks (each with two subtasks, a label and a dependency) with filters. Target: `GET /v1/tasks` with
a filter under 20 ms (median of 30 runs).

Measured 2026-10-01 on a laptop (Intel Core Ultra 5 135U, 16 GB), WSL2 Ubuntu 22.04, release
build, with other agents compiling on the same machine (so worst cases are noisy):

| List (10,010 tasks in the store) | Tasks | Median | Best | Worst |
|---|---|---|---|---|
| `GET /v1/tasks?status=in_progress` (whole response) | 1,671 | 4.8 ms | 3.8 ms | 9.5 ms |
| `GET /v1/tasks?assignee=…&status=todo&status=in_progress` | 835 | 7.7 ms | 5.9 ms | 10.8 ms |
| `GET /v1/tasks?project=…` | 5,007 | 13.3 ms | 10.1 ms | 26.8 ms |
| `GET /v1/tasks` (no filter) | 10,010 | 8.7 ms | 6.6 ms | 16.6 ms |
| `WorkService::tasks`, `status=in_progress` (decoded `Vec<Task>`) | 1,671 | 16.8 ms | 8.1 ms | 27.0 ms |
| `WorkService::tasks`, no filter (decoded) | 10,010 | 37.6 ms | 26.0 ms | 53.6 ms |

The route sends the stored documents as they are; decoding them into `Task`s is what the service
lists cost on top.

## Contract gaps

- `brief_accepted` (and `brief_proposed`) have no `next`, so `PUT /v1/briefs` cannot store the
  next step and seeded briefs lose theirs.
- There is no event for creating or editing a task's other fields (title, description, priority,
  labels, dates, dependencies), nor routes to create projects and workstreams.
