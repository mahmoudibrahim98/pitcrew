# pitcrew-hub-work

The hub's model of the work: projects, workstreams, tasks and subtasks, sessions and dispatches,
asks, comments, briefs, and the workspace's machines, members, personas and teams. Everything is
built from the event log (ADR-0004, ADR-0007).

**Owned by stream E** — see [docs/build/streams/E.md](../../docs/build/streams/E.md).

## Wiring

```rust
let store = Arc::new(Store::open_with(path, StoreOptions::default(), pitcrew_hub_work::projections())?);
// The one writer for this store (see "One writer").
let work = Arc::new(
    WorkService::new(Arc::clone(&store), workspace)      // a protocol `Workspace` (id and name)
        .with_dispatcher(runner_link)                     // Arc<dyn Dispatcher>, stream D
        .with_hub_machine(this_machine),                  // where folderless dispatches run
);
let parts = RouterParts::new()
    .agent(pitcrew_hub_work::agent_routes().layer(Extension(Arc::clone(&work))))
    .device(pitcrew_hub_work::device_routes().layer(Extension(Arc::clone(&work))));
let refs: Arc<dyn pitcrew_hub_work::EventRefs> = work;   // for GET /v1/events filters (stream H)
```

To serve the demo workspace, open an empty store and call
`work.seed(&pitcrew_fixtures::demo_workspace()?)` (the service's workspace must be the demo's).

## One writer

**Exactly one `Arc<WorkService>` per store appends work events.** Build it once and share the
`Arc`: the routes, the runner link and the back office all get clones of it. Nothing else appends
work events to the store.

The service's command lock covers validating a command and appending its events, so two commands
never both pass a check that only one should (two `create_task`s never get the same key; two moves
never both start from the same status). A second writer breaks that, so the projections are also
deterministic under one (tested in `tests/single_writer.rs`):

- a `task_created` whose key another task already holds is **not applied**: the first task keeps
  the key and the refused event is recorded in `work_task_clashes`. The command that lost answers
  `409 conflict`;
- a `task_moved` whose `from` is not the task's status now **is ignored**: it lost a race with
  another move;
- any uniqueness violation that still reaches a command is a `409 conflict`, never a `500`.

## Projections

| Name | Tables (migration) | Events |
|---|---|---|
| `work.directory` | `work_machines`, `work_members`, `work_personas`, `work_teams`, `work_team_members` (0200) | `machine_added`, `machine_liveness`, `member_added`, `persona_saved`, `team_saved` |
| `work.projects` | `work_projects`, `work_project_members`, `work_workstreams`, `work_locations` (0201) | `project_created`, `workstream_created`, `workstream_changed` |
| `work.tasks` | `work_tasks`, `work_subtasks`, `work_task_deps`, `work_task_labels` (0202), `work_task_clashes` (0207) | `task_created`, `task_moved`, `task_assigned`, `subtasks_replaced` |
| `work.sessions` | `work_sessions`, `work_dispatches` (0203) | `session_*`, `turn_ended`, `tool_ran`, `file_edited`, `dispatch_started`, `dispatch_finished` |
| `work.asks` | `work_asks` (0204) | `ask_raised`, `ask_answered` |
| `work.comments` | `work_comments`, `work_comment_mentions` (0205) | `comment_posted` |
| `work.briefs` | `work_briefs`, `work_brief_proposals` (0206) | `brief_proposed`, `brief_accepted` |
| `work.refs` | `work_event_refs`, `work_ref_parents` (0208) | every event about a project, workstream, task or session |

Rules, so that applying events one append at a time and rebuilding give identical tables (tested
in `tests/rebuild.rs`):

- `apply` has no side effects (no appends, no clock) and uses the event's `at`, `author` and
  `on_behalf_of`, never a live caller;
- a projection reads and writes only its own tables; there are no foreign keys between
  projections, and joins happen in reads;
- `apply` fails only on a database error, never on what an event says: an event about something
  unknown (a move of a task the hub never saw) changes nothing, and one that contradicts the
  tables is resolved deterministically (see "One writer"), so the log never stalls;
- a later event that re-states a row (`task_created`, `ask_raised`, `session_discovered` with a
  known id) updates it in place; lists keep the order rows were first created in (`rev`);
- bump a projection's `VERSION` with any change to its `apply` or its tables.

**Tasks.** Each `work_tasks` row keeps the whole task as the API returns it (`doc`, the
protocol's `Task` as JSON) next to the columns lists filter on (project, workstream, assignee,
status, key). A list is one indexed query, and `GET /v1/tasks` sends the stored documents as they
are. Subtasks, dependencies and labels are also indexed one per row, for queries by label, by
blocker or by subtask. Every change rewrites the document and those rows together; the tests check
they agree. Because documents are serialized protocol values, bump `work.tasks`' version when
`Task` changes shape: `tests/task_shape.rs` pins the shape to the version and fails until both
change.

**Sessions: firm links stay.** A link made by a dispatch, a person or the agent itself
(`dispatch`, `manual`, `claimed`) is never replaced by an inferred one (`folder`, `branch`,
`imported`), nor by a re-stated `session_discovered` without a link. A firm link replaces any
link.

**Briefs.** The projection reads only `text` and `pinned` from `brief_accepted`, and derives the
rest: `updated` is the event's time; the source is `back_office` when the back office applied it
(an agent's event) or when the text is exactly the target's latest proposal (a person accepted it
unchanged), and then the proposal's receipts carry over; otherwise it is `person`, with no
receipts. The events now also carry `next` (and `brief_accepted` its receipts), but the projection
does not read them yet, so `next` is always empty and `proposal` absent (see "Contract gaps").

## Activity references (`work.refs`, `EventRefs`)

For every event about a project, workstream, task or session, `work_event_refs` holds all four,
as they were **when the event happened**:

- an event names some itself (a turn its session, a comment its task, a brief its target);
- a session event is about the session's task and workstream as linked then (a later link does
  not reach back; firm links stay, as above);
- a task event is about the task's workstream and project, a workstream event about its project;
- `dispatch_finished` and `ask_answered` are about their dispatch's or ask's task and session.

The projection keeps what it needs for that in its own `work_ref_parents`, because a projection
never reads another's tables. Events about none of them (machines, members, personas, teams) get
no row.

`EventRefs::revs_matching(filter, before_rev, limit) -> (revs, scanned_to)` answers from it, for
`GET /v1/events?project=&workstream=&task=&session=` (stream H wires it; see `src/activity.rs`
for how the route maps it onto the page). It walks the index of the filter's most specific field
and checks the others row by row, examining at most `REF_SCAN_BUDGET` (10,000) rows per call;
`scanned_to` is where to continue, and is 0 exactly when nothing older matches.

## Dispatch

`POST /v1/tasks/{id}/dispatch` (`WorkService::dispatch_task`, `src/dispatch.rs`):

1. checks the request (`404` task; `400` agent, machine, or a person as the agent; `409` done or
   canceled; `503` no live machine) and appends, in one transaction: `task_assigned` to the agent
   if the task has none, `dispatch_started` (naming a new session id), and `session_discovered`
   for that session (state `starting`, `link_basis: dispatch`);
2. calls `Dispatcher::start` with a `DispatchRequest` (everything decided: machine, folder, engine,
   persona, model, permission mode, brief, the session id), without the command lock;
3. if that fails, appends `dispatch_finished` (outcome `failed`, the reason as the summary) and
   `session_ended`, and answers `503`, `409` or `500`. A panicking dispatcher counts as failed.

The machine is the request's, else the machine of the task's workstream's first location, else the
project's root's, else the hub's own (`with_hub_machine`, or the first `local` machine). The folder
is the first of those locations on that machine, else `~`. The engine, model and permission mode
come from the agent's persona (Claude Code by default). Without a dispatcher the route answers
`503`.

## Commands

`WorkService` validates each command against the tables, then appends its events in one
transaction, stamped from the `Caller` (`author` = the caller; `on_behalf_of` = the owner, for
agents only). Commands run one at a time; reads never wait for them.

Authorization comes before validation: the order is `404` for what the path names, `403`, `400`
for the body, then `409`. The agent routes check who may write before they read the body, so a
forbidden agent hears `403` whatever it sent.

| Command | Who | Refusals |
|---|---|---|
| `create_task` | person | 400 empty title, unknown project/workstream/assignee, workstream of another project, bad date; 409 key taken by a racing writer |
| `move_task` | person; agent on its own task | 404; 403 agent on another's task; 409 when `can_move` says no |
| `assign_task` | person | 404; 400 unknown member |
| `replace_subtasks` | person (whole list); agent on its own task (only its own `agent_plan` lines, in place) | 404; 403; 400 empty text, repeated ids, `agent_plan` naming a non-agent |
| `post_comment` | person; agent on its own task | 404; 403; 400 empty text, unknown mention |
| `raise_ask` | anyone; an agent only about its own task and session | 400 unknown task or session; 403; 400 empty title, unknown addressee |
| `answer_ask` | the addressee, or a person for their agents; decisions, approvals and reviews only by people | 404; 403; 400 no option and no non-blank text, option out of range; 409 already answered |
| `put_brief` | person | 404 unknown target |
| `patch_workstream` | person | 404; 400 empty patch |
| `dispatch_task` | person | see "Dispatch" |
| `dispatch_working` | the runner link | moves the dispatched task to in progress as the agent, when the rules allow |
| `mirror_plan` | the runner link | replaces the agent's `agent_plan` lines of its own task from a `PlanUpdated` |
| `seed` | the daemon | imports a `DemoWorkspace` into an empty work model |

"Own task" means the agent is the assignee or holds an active (not ended) dispatch on it.

## Routes

Agent and device tokens (`agent_routes`): `GET /v1/me`, `GET /v1/members`,
`GET /v1/tasks?project=&workstream=&assignee=&status=`, `GET /v1/tasks/{id-or-key}`,
`POST /v1/tasks/{id}/move`, `PUT /v1/tasks/{id}/subtasks`, `POST /v1/tasks/{id}/comments`,
`GET /v1/asks?to=&state=`, `POST /v1/asks`, `POST /v1/asks/{id}/answer`.

Device tokens only (`device_routes`): `GET /v1/workspace`, `GET /v1/machines`,
`GET /v1/personas`, `GET /v1/teams`, `GET /v1/projects`, `GET /v1/projects/{id}`,
`GET /v1/workstreams?project=`, `GET|PATCH /v1/workstreams/{id}`, `POST /v1/tasks`,
`POST /v1/tasks/{id}/assign`, `POST /v1/tasks/{id}/dispatch`,
`GET /v1/sessions?machine=&workstream=&task=&state=`, `GET /v1/sessions/{id}`,
`GET /v1/briefs`, `PUT /v1/briefs/{project|workstream}/{id}`.

`GET /v1/workspace` answers `{ workspace, rev }`; `rev` is the lowest checkpoint of the work
projections (`projection_state.rev`), the revision every work table reflects.

Errors are `ApiError` bodies: a malformed id or unknown thing in the path is `404`, in the body or
query `400`; bodies over 1 MiB are `400`. A `500` is logged in full and its body always says
`INTERNAL_MESSAGE`: no SQLite text, table or constraint names reach a client.

## Tests

- `tests/seed.rs`: after `seed`, every list and item route equals the demo fixture structurally
  (briefs differ only by `next`). The ignored `seeded_routes_answer_like_the_running_mock_hub`
  compares against the mock hub's own answers: dump them with
  `node crates/hub-work/tests/dump-mock-hub.mjs <folder>`, then run it with
  `PITCREW_MOCK_DUMP=<folder>`.
- `tests/rebuild.rs`: rebuilding every projection, building them on open, and applying one event
  per append all give identical tables, over every table including the new ones.
- `tests/single_writer.rs`: key clashes, stale moves, catch-up on open, concurrent `create_task`,
  a racing writer's `409`, keys by prefix, `on_behalf_of`, internal errors.
- `tests/rules.rs`: the whole move-rules table through the routes, agent scope (every device route,
  mounted bare and guarded), authorization before validation, who answers asks.
- `tests/refs.rs`: the reference index against an independent oracle over the demo, paging,
  the scan budget, links over time, the query plans.
- `tests/sessions.rs`, `tests/dispatch.rs`, `tests/routes.rs`, `tests/self_moving.rs`: the other
  routes and commands. `tests/task_shape.rs`: the `Task` shape pin.

## Timings

`cargo test -p pitcrew-hub-work --release --test perf -- --ignored --nocapture` lists 10,000
tasks (each with two subtasks, a label and a dependency) with filters, and pages the activity
index over the same log. Target: `GET /v1/tasks` with a filter under 20 ms (median of 30 runs);
the test asserts it for the status and assignee filters, and reports the rest.

Measured 2026-10-01 on a laptop (Intel Core Ultra 5 135U, 16 GB), WSL2 Ubuntu 22.04, release
build, with other agents compiling on the same machine (so the numbers are noisy):

| List (10,010 tasks in the store) | Rows | Median | Best | Worst |
|---|---|---|---|---|
| `GET /v1/tasks?status=in_progress` (whole response) | 1,671 | 8.2 ms | 5.9 ms | 16.4 ms |
| `GET /v1/tasks?assignee=…&status=todo&status=in_progress` | 835 | 8.4 ms | 6.3 ms | 10.1 ms |
| `GET /v1/tasks?project=…` (half the workspace) | 5,007 | 24.1 ms | 16.0 ms | 51.0 ms |
| `GET /v1/tasks` (no filter) | 10,010 | 9.4 ms | 7.6 ms | 19.5 ms |
| `WorkService::tasks`, `status=in_progress` (decoded `Vec<Task>`) | 1,671 | 25.4 ms | 10.7 ms | 36.2 ms |
| `WorkService::tasks`, no filter (decoded) | 10,010 | 41.8 ms | 27.1 ms | 145.5 ms |
| `EventRefs::revs_matching`, project, newest 100 | 100 events | 0.23 ms | 0.19 ms | 0.40 ms |
| `EventRefs::revs_matching`, project, every page of 500 | 5,036 events | 9.8 ms | 5.9 ms | 15.6 ms |

The route sends the stored documents as they are; decoding them into `Task`s is what the service
lists cost on top. An earlier run on a quieter machine measured about half these medians for the
task lists.

## Contract gaps

The contract change `integrator/work-edits` (merged) added these; this crate does not implement
them yet:

- `next` on `brief_proposed` and `brief_accepted`, receipts on `brief_accepted`, and
  `Brief.proposal`: `PUT /v1/briefs` writes `next` into the event, but the briefs projection does
  not read it, so briefs answer without `next` or `proposal`, and seeded briefs lose their next
  step.
- `task_updated` and `PATCH /v1/tasks/{id-or-key}`: the tasks projection ignores the event. Only
  `work.refs` follows it (a task moved to another workstream counts there from then on).
- `POST /v1/projects` and `POST /v1/workstreams`. Until they land, nothing makes project keys
  unique; task keys are allocated by key prefix, so two projects that share a key still never
  share a task key.
