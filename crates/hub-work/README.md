# pitcrew-hub-work

The hub's model of the work: projects, workstreams, tasks and subtasks, sessions and dispatches,
asks, comments, briefs, and the workspace's machines, members, personas and teams. Everything is
built from the event log (ADR-0004, ADR-0007).

**Owned by stream E** — see [docs/build/streams/E.md](../../docs/build/streams/E.md).

## Wiring

```rust
// The back office acts as its own agent member of the workspace (`@office` in the demo).
let office = Arc::new(BackOffice::new(office_member));    // Config::office = office_member
let store = Arc::new(Store::open_with(
    path,
    StoreOptions::default(),
    pitcrew_hub_work::projections_with_office(&office),   // the work tables + the office's run log
)?);
let mut appended = store.subscribe();                     // before anything appends
// The one writer for this store (see "One writer").
let work = Arc::new(
    WorkService::new(Arc::clone(&store), workspace)      // a protocol `Workspace` (id and name)
        .with_dispatcher(runner_link)                     // Arc<dyn Dispatcher>, stream D
        .with_hub_machine(this_machine),                  // where folderless dispatches run
);
let parts = RouterParts::new()
    .agent(pitcrew_hub_work::agent_routes().layer(Extension(Arc::clone(&work))))
    .device(pitcrew_hub_work::device_routes().layer(Extension(Arc::clone(&work))));
let refs: Arc<dyn pitcrew_hub_work::EventRefs> = work.clone(); // GET /v1/events filters (stream H)

// The back office: after each append batch, apply what it emitted (see "The back office").
tokio::spawn(async move {
    let mut last = store.latest_rev()?;                   // never re-apply older batches
    loop {
        let revs = match appended.recv().await {
            Ok(revs) if revs.to_rev <= last => continue,
            Ok(revs) => RevRange { from_rev: revs.from_rev.max(last + 1), to_rev: revs.to_rev },
            Err(RecvError::Lagged(_)) => RevRange { from_rev: last + 1, to_rev: store.latest_rev()? },
            Err(RecvError::Closed) => break,
        };
        last = revs.to_rev;
        let (work, office) = (Arc::clone(&work), Arc::clone(&office));
        let run = tokio::task::spawn_blocking(move || work.run_office(&office, revs)).await??;
        // `run.refused()`: actions the hub would not apply (already logged as warnings).
    }
});
```

(Errors elided: the loop logs a failed `run_office` and goes on.) Without a back office, open the
store with `pitcrew_hub_work::projections()` and skip the loop.

To serve the demo workspace, open an empty store and call
`work.seed(&pitcrew_fixtures::demo_workspace()?)` (the service's workspace must be the demo's),
then `work.run_office(&office, seeded)` with the revisions `seed` returns, if the loop above was
not running yet.

## One writer

**Exactly one `Arc<WorkService>` per store appends work events.** Build it once and share the
`Arc`: the routes, the runner link and the back office all get clones of it. Nothing else appends
work events to the store.

The service's command lock covers validating a command and appending its events, so two commands
never both pass a check that only one should (two `create_task`s never get the same key; two moves
never both start from the same status). A second writer breaks that, so the projections are also
deterministic under one (tested in `tests/single_writer.rs`):

- a `task_created` whose key another task already holds is **not applied**: the first task keeps
  the key and the refused event is recorded in `work_task_clashes` (and `work.refs` ignores it
  too). The command that lost answers `409 conflict`;
- a `project_created` whose key another project already holds is **not applied** either (project
  keys are unique, migration 0209): the first project keeps the key, and `create_project` answers
  `409 conflict` when it lost the race;
- a `task_moved` whose `from` is not the task's status now **is ignored**: it lost a race with
  another move. `move_task` and `dispatch_working` check where the task ended up after their
  append, and answer `409 conflict` when it is not where they moved it;
- a `UNIQUE` or `PRIMARY KEY` violation that still reaches a command is a `409 conflict`, never a
  `500`. Any other constraint violation (`NOT NULL`, `CHECK`, a foreign key) is a bug in a
  projection: a `500`, logged as an error.

## Projections

| Name | Tables (migration) | Events |
|---|---|---|
| `work.directory` | `work_machines`, `work_members`, `work_personas`, `work_teams`, `work_team_members` (0200) | `machine_added`, `machine_liveness`, `member_added`, `persona_saved`, `team_saved` |
| `work.projects` | `work_projects`, `work_project_members`, `work_workstreams`, `work_locations` (0201; unique keys 0209) | `project_created`, `workstream_created`, `workstream_changed` |
| `work.tasks` | `work_tasks`, `work_subtasks`, `work_task_deps`, `work_task_labels` (0202), `work_task_clashes` (0207) | `task_created`, `task_moved`, `task_assigned`, `task_updated`, `subtasks_replaced` |
| `work.sessions` | `work_sessions`, `work_dispatches` (0203) | `session_*`, `turn_ended`, `tool_ran`, `file_edited`, `dispatch_started`, `dispatch_finished` |
| `work.asks` | `work_asks` (0204) | `ask_raised`, `ask_answered` |
| `work.comments` | `work_comments`, `work_comment_mentions` (0205) | `comment_posted` |
| `work.briefs` | `work_briefs`, `work_brief_proposals` (0206; a proposal's `next` 0210) | `brief_proposed`, `brief_accepted` |
| `work.refs` | `work_event_refs`, `work_ref_parents` (0208) | every event about a project, workstream, task or session |

With a back office, `projections_with_office` adds stream F's `office.runs` (`office_runs`,
`office_state`, migration 0301); see "The back office".

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
link. Agents stay the same way: a re-stated session without an `agent` keeps the one it had.

**Tasks: `task_updated`.** The projection writes the event's patch into the task as it is
(`TaskPatch::apply`) and rewrites the filter columns and child rows with it, so a task moved to
another workstream lists there at once. `PATCH /v1/tasks` checked the rules before it appended.

**Briefs.** The brief in force is the one the newest `brief_accepted` put there, with the `text`,
`next`, `pinned` and `receipts` the event carries, and `updated` its time. `work_brief_proposals`
holds each target's **pending** proposal only: a `brief_proposed` replaces the target's row and a
`brief_accepted` removes it, so a proposal is pending exactly while it is newer than the brief in
force. `GET /v1/briefs` joins it in as `proposal`; a target with a proposal but no brief in force
is not listed. A `brief_accepted` is the back office's (`source: back_office`) when its text and
next step both equal the pending proposal's (a missing `next` equals only a missing `next`), or
when an agent wrote it (`on_behalf_of` is set: the back office applying a brief itself);
otherwise it is the person's.

## Activity references (`work.refs`, `EventRefs`)

For every event about a project, workstream, task or session, `work_event_refs` holds all four,
as they were **when the event happened**:

- an event names some itself (a turn its session, a comment its task, a brief its target);
- a session event is about the session's task and workstream as linked then (a later link does
  not reach back; firm links stay, as above);
- a task event is about the task's workstream and project, a workstream event about its project;
- `dispatch_finished` and `ask_answered` are about their dispatch's or ask's task and session.

The projection keeps what it needs for that in its own `work_ref_parents`, because a projection
never reads another's tables, including which task holds each key, so that a `task_created`
refused for a key clash changes no task's parents here either. Events about none of them
(machines, members, personas, teams) get no row.

`EventRefs::revs_matching(filter, before_rev, limit) -> (revs, scanned_to)` answers from it. It
is for the `project=` and `workstream=` filters of `GET /v1/events`, which need the roll-up
(stream H wires it; see `src/activity.rs` for how the route maps it onto the page). The index
keeps one task and one session per event, the event's own, so `task=` and `session=` keep
stream H's matching of ids anywhere in the event body, which also catches ids an event names
elsewhere. It walks the index of the filter's most specific field and checks the others row by
row, examining at most `REF_SCAN_BUDGET` (10,000) rows per call. `scanned_to` is where to
continue; it is 0 only when the search reached the start of the log (nothing older matches), and
a non-zero one does not promise older matches.

## Dispatch

`POST /v1/tasks/{id}/dispatch` (`WorkService::dispatch_task`, `src/dispatch.rs`):

1. checks the request (`404` task, before the body is read; `400` agent, machine, or a person as
   the agent; `409` done or canceled, or the agent already holds an active dispatch on the task,
   such as a second click; `503` no live machine) and appends, in one transaction:
   `task_assigned` to the agent if the task has none, `dispatch_started` (naming a new session
   id), and `session_discovered` for that session (state `starting`, `link_basis: dispatch`);
2. calls `Dispatcher::start` with a `DispatchRequest` (everything decided: machine, folder, engine,
   persona, model, permission mode, brief, the session id), without the command lock;
3. if that fails, logs why, appends `dispatch_finished` (outcome `failed`, the reason as the
   summary) and `session_ended`, and answers `503`, `409` or `500`. A panicking dispatcher counts
   as failed.

The machine is the request's, else the machine of the task's workstream's first location, else the
project's root's, else the hub's own. **The daemon must name the hub's machine** with
`with_hub_machine`; without it, a dispatch with nowhere else to run answers `503` (the hub does
not guess one of the workspace's machines). The folder is the first of those locations on that
machine, else `~`. The engine, model and permission mode come from the agent's persona (Claude
Code by default). Without a dispatcher the route answers `503`.

**For the runner link (stream D):** if the hub stops between step 1 and step 3, a `starting`
session and an open dispatch are left behind. The runner link must reconcile them: on start-up,
and when a start is not confirmed within its timeout, end the session and finish the dispatch as
`failed`, or report the session it did start under the `DispatchRequest`'s session id.

## Commands

`WorkService` validates each command against the tables, then appends its events in one
transaction, stamped from the `Caller` (`author` = the caller; `on_behalf_of` = the owner, for
agents only). Commands run one at a time; reads never wait for them.

Authorization comes before validation: the order is `404` for what the path names, `403`, `400`
for the body, then `409`. Routes with a path and a body look up what the path names and check who
may write before they read the body, so an unknown task is `404` and a forbidden agent hears `403`
whatever it sent.

| Command | Who | Refusals |
|---|---|---|
| `create_task` | person | 400 empty title, unknown project/workstream/assignee, workstream of another project, bad date; 409 key taken by a racing writer |
| `patch_task` | person | 404; 400 title not 1–500 characters trimmed, a label not 1–64 characters trimmed or over 32 labels, workstream unknown or of another project, blocker unknown or the task itself, malformed date, start after due (as the task will be); then 409 `blocked_by` cycle. Appends `task_updated` with only the changed fields, or nothing |
| `create_project` | person | 400 blank name, unknown lead or member, malformed date, start after due, root on an unknown machine or with a blank path; then 409 key in use (or taken by a racing writer) |
| `create_workstream` | person | 400 blank name, location on an unknown machine or with a blank path; then 404 unknown project (it is in the body, as api-v1 says) |
| `move_task` | person; agent on its own task | 404; 403 agent on another's task; 409 when `can_move` says no, or a racing writer moved the task first |
| `assign_task` | person | 404; 400 unknown member |
| `replace_subtasks` | person (whole list); agent on its own task (only its own `agent_plan` lines, in place) | 404; 403; 400 empty text, repeated ids, `agent_plan` naming a non-agent |
| `post_comment` | person; agent on its own task | 404; 403; 400 empty text, unknown mention |
| `raise_ask` | anyone; an agent only about its own task and session | 400 unknown task or session; 403; 400 empty title, unknown addressee |
| `answer_ask` | the addressee, or a person for their agents; decisions, approvals and reviews only by people | 404; 403; 400 no option and no non-blank text, option out of range; 409 already answered |
| `put_brief` | person | 404 unknown target. With the pending proposal's text and next, `brief_accepted` carries its receipts (and the brief is the back office's) |
| `patch_workstream` | person | 404; 400 empty patch |
| `dispatch_task` | person | see "Dispatch" |
| `dispatch_working` | the runner link | moves the dispatched task to in progress as the agent, when the rules allow; 409 for an ended dispatch, or a racing writer that moved the task first |
| `mirror_plan` | the runner link | replaces the agent's `agent_plan` lines of its own task from a `PlanUpdated` |
| `seed` | the daemon | imports a `DemoWorkspace` into an empty work model |
| `run_office`, `OfficeCommands` | the back office | see "The back office" |

"Own task" means the agent is the assignee or holds an active (not ended) dispatch on it.

Every `400` comes before a `409`: a request that is malformed and would also conflict is a `400`.
Object bodies must be JSON objects (serde would otherwise read an array as a struct's fields by
position); `PUT /v1/tasks/{id}/subtasks` takes an array.

## The back office

Stream F's office (`crates/office`) decides; this crate applies what it decided, as the hub.

**How it runs.** The office's rules run inside the store, as its run log: `BackOffice` holds the
office's member, `Config` (`Config::office` = that member) and rules, and
`projections_with_office(&office)` registers `office.run_log()`, a `pitcrew_office::RunLog` with
that same `Config`. The store applies the run log to every append in the append's transaction:
each action a rule takes is logged in `office_runs` as `emitted`, `capped` or `refused` (by the
office's "never" list and move rules). So the run log *is* the live office; there is no second
office whose state could disagree with what it logged.

**The entry point.** After an append batch commits, the daemon calls
`WorkService::run_office(&office, revs)` with the batch's revisions (see "Wiring"). It reads the
run log's entries for those revisions and hands the emitted ones, in log order and whole
revisions at a time, to `pitcrew_office::apply`, which re-checks their shape and calls
`OfficeCommands`. Capped and refused entries are only logged. It returns every emitted action with
its result; a refusal appends nothing and is logged as a warning. It fails, applying nothing, when
the run log has not reached `revs.to_rev` (it is not registered) or the office's member is not an
agent of the workspace.

**`OfficeCommands`** (`pitcrew_office::Commands` over `WorkService`) re-validates every action
against the hub's tables, like any caller's (`apply` checks only shape), and appends its events
authored by the office's member, on behalf of its owner:

| Action | Applied when | Else |
|---|---|---|
| `TaskMoved` | the mover is `Mover::BackOffice` (recorded with the task's own `accept_auto`, whatever the action claimed), `from` is the task's status, and `can_move` allows it, so review → done only with `accept_auto` | 403 another mover; 404 unknown task; 409 |
| `AskAnswered` | an open question or mention addressed to the office or another agent of its owner; the answer is the office's (`by`, `at`) | 403 a person's ask, a decision, approval or review, another owner's agent; 404; 400 bad option or no answer; 409 answered |
| `CommentPosted` | on a known task or workstream, known mentions, text not blank | 404; 400 |
| any other event | never | 403 |
| `raise_ask` | a known addressee, task and session; not an approval | 403 approval; 400 |
| `propose_brief` | a known target: `brief_proposed`, and with it `accepted_body()` when the proposal says auto-accept, unless the brief in force is pinned (then only proposed) | 404 |

Every action must cite receipts (400 without). The office's member must be an agent; a person
cannot be the back office.

**What the daemon must know.**
- Call `run_office` once per batch. Replaying a batch re-applies its actions (a move is refused as
  stale, but an ask or comment is raised again), so after a restart start from the store's latest
  revision, as the loop in "Wiring" does.
- A crash between an append and its `run_office` loses that batch's actions; they stay in the run
  log as emitted. Recovering them needs a record of what was applied, which nothing in the log
  holds yet.
- An action the office emitted and the hub refused (the office's view was out of date, e.g. after
  a racing writer's stale move) is `emitted` in `office_runs`: a projection cannot record what the
  hub later did. `run_office` returns and logs the refusal.

## Routes

Agent and device tokens (`agent_routes`): `GET /v1/me`, `GET /v1/members`,
`GET /v1/tasks?project=&workstream=&assignee=&status=`, `GET /v1/tasks/{id-or-key}`,
`POST /v1/tasks/{id}/move`, `PUT /v1/tasks/{id}/subtasks`, `POST /v1/tasks/{id}/comments`,
`GET /v1/asks?to=&state=`, `POST /v1/asks`, `POST /v1/asks/{id}/answer`.

Device tokens only (`device_routes`): `GET /v1/workspace`, `GET /v1/machines`,
`GET /v1/personas`, `GET /v1/teams`, `GET|POST /v1/projects`, `GET /v1/projects/{id}`,
`GET /v1/workstreams?project=`, `POST /v1/workstreams`, `GET|PATCH /v1/workstreams/{id}`,
`POST /v1/tasks`, `PATCH /v1/tasks/{id-or-key}`, `POST /v1/tasks/{id}/assign`,
`POST /v1/tasks/{id}/dispatch`, `GET /v1/sessions?machine=&workstream=&task=&state=`,
`GET /v1/sessions/{id}`, `GET /v1/briefs`, `PUT /v1/briefs/{project|workstream}/{id}`.

`GET /v1/workspace` answers `{ workspace, rev }`; `rev` is the lowest checkpoint of the work
projections (`projection_state.rev`), the revision every work table reflects.

Errors are `ApiError` bodies: a malformed id or unknown thing in the path is `404`, in the body or
query `400`; bodies over 1 MiB are `400`. A `500` is logged in full and its body always says
`INTERNAL_MESSAGE`: no SQLite text, table or constraint names reach a client.

## Tests

- `tests/seed.rs`: after `seed`, every list and item route equals the demo fixture structurally,
  briefs included (with the demo's pending proposal for the paper). Two ignored tests compare with
  the mock hub's own answers: dump them with
  `node crates/hub-work/tests/dump-mock-hub.mjs <folder>`, then run
  `PITCREW_MOCK_DUMP=<folder> cargo test -p pitcrew-hub-work --test seed -- --ignored`.
  `seeded_routes_answer_like_the_running_mock_hub` compares every GET route (only the
  workspace's `rev` differs); `writes_answer_like_the_running_mock_hub` replays the 110 write
  requests the script made on the mock (every rule and error code of `PATCH /v1/tasks`,
  `POST /v1/projects`, `POST /v1/workstreams`, and briefs' next steps and proposals) and compares
  statuses, error codes and bodies, with created ids matched up and brief times left out.
- `tests/edits.rs`, `tests/briefs.rs`: every rule and error code of the three routes and of
  briefs, through the routes (the cases of the mock hub's `edits.test.ts`), and project key
  clashes from a racing writer.
- `tests/office.rs`: a finished dispatch moves its task to review; actions the office's guard
  refuses are logged `refused` and refused by the hub too; an action emitted on an out-of-date
  view is refused by the hub; every (from, to, mover) through `OfficeCommands` against
  `can_move`; answers, asks, comments and brief proposals (pinned and not); a run log several
  pages long applied whole, once, in order.
- `tests/rebuild.rs`: rebuilding every projection, building them on open, and applying one event
  per append all give identical tables, over every table, with task edits, key clashes, new
  projects and workstreams, and pending and accepted proposals in the log.
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
build, with several other agents compiling on the same machine (so the numbers are noisy), after
`task_updated` and the brief proposals landed:

| List (10,010 tasks in the store) | Rows | Median | Best | Worst |
|---|---|---|---|---|
| `GET /v1/tasks?status=in_progress` (whole response) | 1,671 | 5.7 ms | 5.0 ms | 31.2 ms |
| `GET /v1/tasks?assignee=…&status=todo&status=in_progress` | 835 | 14.8 ms | 6.7 ms | 24.6 ms |
| `GET /v1/tasks?project=…` (half the workspace) | 5,007 | 22.8 ms | 17.6 ms | 34.1 ms |
| `GET /v1/tasks` (no filter) | 10,010 | 11.6 ms | 8.4 ms | 72.7 ms |
| `WorkService::tasks`, `status=in_progress` (decoded `Vec<Task>`) | 1,671 | 20.4 ms | 14.0 ms | 38.7 ms |
| `WorkService::tasks`, no filter (decoded) | 10,010 | 67.7 ms | 48.7 ms | 138.6 ms |
| `EventRefs::revs_matching`, project, newest 100 | 100 events | 0.20 ms | 0.15 ms | 2.51 ms |
| `EventRefs::revs_matching`, project, every page of 500 | 5,037 events | 18.3 ms | 8.5 ms | 25.8 ms |

The route sends the stored documents as they are; decoding them into `Task`s is what the service
lists cost on top. An earlier run on a quieter machine measured about half the task lists'
medians.

## Differences from the mock hub

- The mock accepts the display form of an id in a body (`"project": "prj_01JB…"` in
  `POST /v1/workstreams`). api-v1 says ids are bare ULIDs, and the hub answers `400` for any
  other form in a body (paths take both).
