# pitcrew-hub-work

Session import stores reversible rules via `with_import_file` in a private atomic file. API session reads filter the retained projections; internal runner/command reads remain complete. Recaps rebuild when the choice changes, retaining the original event log.

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
// Subscribe, then read where the log is, now, before anything appends (the seed included): every
// later revision is the back office's to look at.
let mut appended = store.subscribe();
let mut last = store.latest_rev()?;
// The one writer for this store (see "One writer").
let work = Arc::new(
    WorkService::new(Arc::clone(&store), workspace)      // a protocol `Workspace` (id and name)
        .with_dispatcher(runner_link)                     // Arc<dyn Dispatcher>, stream D
        .with_hub_machine(this_machine)                   // where folderless dispatches run
        .with_setup_listener(setup_listener)               // starts the office and runner on setup
        .with_recap_file(state_dir.join("recaps.sqlite3")), // the recap blocks on disk (see "Recaps")
);
let parts = RouterParts::new()
    .agent(pitcrew_hub_work::agent_routes().layer(Extension(Arc::clone(&work))))
    .device(pitcrew_hub_work::device_routes().layer(Extension(Arc::clone(&work))));
let refs: Arc<dyn pitcrew_hub_work::EventRefs> = work.clone(); // GET /v1/events filters (stream H)
let recaps: Arc<dyn pitcrew_hub_work::RecapIndex> = work.clone(); // GET /v1/recaps/* (stream H)
// Optional: build the recap index now, off the request path (it reads the whole log once).
tokio::task::spawn_blocking({
    let work = Arc::clone(&work);
    move || {
        let started = std::time::Instant::now();
        match work.sync_recaps() {
            Ok(rev) => tracing::info!(rev, ms = started.elapsed().as_millis(), "recap index built"),
            // Not fatal: the first recap request reads the log again from where this stopped.
            Err(error) => tracing::warn!(%error, ms = started.elapsed().as_millis(),
                "building the recap index failed"),
        }
    }
});

// The back office: after each append batch, apply what it emitted (see "The back office").
tokio::spawn(async move {
    loop {
        let to_rev = match appended.recv().await {
            Ok(revs) => revs.to_rev,
            Err(RecvError::Lagged(_)) => store.latest_rev()?,
            Err(RecvError::Closed) => break,
        };
        if to_rev <= last {
            continue;
        }
        // From the first revision not looked at: this also covers revisions another process
        // appended, which `subscribe` does not announce.
        let revs = RevRange { from_rev: last + 1, to_rev };
        let (work, office) = (Arc::clone(&work), Arc::clone(&office));
        let run = tokio::task::spawn_blocking(move || work.run_office(&office, revs)).await??;
        last = to_rev;
        // `run.refused()`: actions the hub would not apply (already logged as warnings).
    }
});
```

(Errors elided: on a failed `run_office` the loop logs it and tries the same range again later;
re-running a range is safe.) Without a back office, open the store with
`pitcrew_hub_work::projections()` and skip the loop.

To serve the demo workspace, open an empty store, start the loop, and call
`work.seed(&pitcrew_fixtures::demo_workspace()?)` (the service's workspace must be the demo's);
the loop applies what the office makes of the seed like any other batch.

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

Read cursors use `cursor_moved` events and the `work.cursors` projection
(`work_read_cursors`, migration 0211). The event author owns the cursor; the projection
keeps the maximum revision for each author and scope, so rebuilding never moves it back.
`GET /v1/me/cursors` and `PUT /v1/me/cursors/{scope}` are device-only routes, also checked
by the service. Commands serialize validation and append, reject future revisions and
unknown scopes, and append nothing for an equal or older revision. This follows the same
event-based storage as the work model. Cursor events are private: only the author's
devices receive live or replayed cursor events; activity routes and recap inputs exclude them. `tests/cursors.rs` covers isolation, refusals, no-op writes and replay.

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
(`dispatch`, `manual`, `claimed`, `imported`) is never replaced by an inferred one (`folder`,
`branch`), nor by a re-stated `session_discovered` without a link. A firm link replaces any
link. Agents stay the same way: a re-stated session without an `agent` keeps the one it had.
An ended session stays ended (projection version 3): a re-stated `session_discovered` keeps its
state and status line, so a CLI that turns up after the hub ended its session (it decided the CLI
had not started) does not bring the session back. A later `session_state_changed` still applies.

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

## Recaps (`RecapIndex`)

`GET /v1/recaps/blocks` and `GET /v1/recaps/days` (api-v1, "Recaps") are answered by the recap
index, `src/recap.rs`. Stream H's routes call it through a seam the daemon adapts, as for
`EventRefs`:

```rust
// pitcrew_hub_work::RecapIndex, implemented by WorkService. Both block: call them on the blocking
// pool.
fn recap_blocks(&self, filter: &BlockFilter, before: Option<EventId>, limit: Option<usize>)
    -> Result<BlocksPage>;
fn recap_days(&self, scope: DaysScope, tz_minutes: i32, before: Option<&Date>,
    limit: Option<usize>) -> Result<DaysPage>;
// BlockFilter { session, task, workstream, project }: all Option, combined; none = every block.
// DaysScope::Workstream(id) | DaysScope::Project(id).
```

- **What it serves.** Blocks newest first by id, each with its `block_line`; `before` exclusive,
  any event id. Days: `day_recaps` with the `RuleSummarizer` over the blocks whose `workstream`
  (or `project`) it is, newest date first and, within a date, the entry without a workstream first,
  then by workstream id; pages hold whole dates. `limit: None` is the protocol's default (50 blocks,
  7 dates) and more than the cap (200, 30) counts as the cap. A page that is not at the start is
  never empty; an unknown id is an empty page with `at_start`. What the contract calls `400`
  (`limit` 0, `tz` beyond ±840, a `before` that is not `YYYY-MM-DD`) is `invalid` here too, so the
  seam never panics whatever reaches it.
- **For the daemon's adapter.** Pass `Some(limit)`: the route has applied the default and the cap
  already. Copy the filter's four fields, map the scope variant for variant, pass `before.as_ref()`
  for days, and convert the error as for `EventRefs`. Once the route has validated, an `invalid`
  from the index means the two disagree about the contract: a bug, worth logging as one.
- **Kept current on read.** `Recaps` (the index itself; it reads no store and no clock) feeds the
  recap engine's `BlockBuilder` every event in log order and keeps every block, open and closed,
  indexed by session, task, workstream and project (see "Where the blocks are"). The service's
  `Recaps` starts empty: **every query
  first reads the log from the last revision the index applied** (`Store::since`, 1,000 events at a
  time, each page applied whole or not at all), then answers. That is the back office's "from the
  first revision not looked at" without a subscription: no event is missed or applied twice,
  appends by another process included, and a client that refetches after a stream frame gets the
  frame's event. The first query builds the index from the whole log;
  `WorkService::sync_recaps()` does it eagerly (see "Wiring") and returns the revision it reflects.
- **From the log, not from the projections.** The engine's `Directory` (which session works on
  which task, which task is in which workstream, and members for names) starts empty and learns
  from the events from revision 1, so a block's links are those in force when its events happened,
  and an index kept current for months equals one rebuilt today. Seeding it with today's
  projections would put today's links ahead of yesterday's events. One directory serves both
  grouping and names; `Recaps` keeps no second copy.
- **A refused `task_created`** (in `work_task_clashes`, see "One writer") is not activity: it is
  left out, so a task the hub never had is in no recap. Whether one was refused is known only once
  the tasks projection has applied it, so the index reads no further than that projection's
  revision (read in the same snapshot as the clashes). When another process appended without the
  work model's projections, the rest waits until this store's next append catches them up, and
  `sync_recaps` returns the projection's revision meanwhile.
- **An engine failure is one event, not the index.** An event the recap engine panics on is left
  out, logged as an error and counted (`Recaps::failed_events`); a rebuild leaves it out the same
  way. A panic while writing a line or a day paragraph is an internal error for that request; the
  index is not poisoned, so requests do not rebuild it.
- **Day paragraphs are cached** by scope, `tz`, date and workstream, with the `(id, last)` of every
  block each covers. A query writes a paragraph again only when its blocks changed (one grew, began,
  or moved to another day or workstream), so a growing block rewrites only its own day; or when a
  name it may show changed: a member, task or workstream renamed, an ask re-stated as another kind
  or by another asker, a name dropped for the directory's bound (`Directory::names_version`,
  past the 100,000 of each kind it keeps), or a name learned for something a block already named
  while it was unknown ("a task" becomes "PAP-9"). Then every paragraph is written again. A new
  member, task or ask that nothing named yet changes no paragraph. At most `DAY_CACHE_ENTRIES`
  (2,048) are kept, least recently used out first.
- **Where the blocks are.** The engine keeps in memory only what it needs to go on: its directory
  and the blocks still open. Every block made so far, open or closed, is written as it begins or
  changes to a SQLite database of the index's own (`src/recap_db.rs`), never to `hub.db`: one row
  a block (its JSON, its start, and the session, workstream and project it is linked to), its
  tasks in a table of their own, and a partial index for each filter, so a query reads only its
  own blocks, in the order the in-memory sets gave (by id, newest first; a scope's by
  `(start, id)` for days). Each batch of blocks is one transaction. `Recaps::new` keeps the
  database in memory; `WorkService::with_recap_file(path)`, which the daemon uses, in a file, so a
  long history costs disk instead. On a local filesystem the file is at the requested path.
  On a network or unknown filesystem it goes in a private local folder: the temporary folder
  first, then `$XDG_RUNTIME_DIR`, skipping unsuitable or networked bases. If neither works,
  blocks stay in memory. On Unix the folder's name hashes the canonical requested path and uid;
  a leftover is reused only if `lstat` shows a real directory owned by this user with mode 0700.
  Otherwise (and on Windows) a random folder is used. A clean stop removes the file and folder;
  a hard kill leaves them behind. The next start at the same Unix path replaces the stable
  folder's cache; random leftovers remain until the temporary folder is cleaned.
  P-measure's history (`benches/README.md`, "At scale": 612,000
  events, 10,000 sessions) makes 10,000 blocks, a 33 MiB file; with the blocks in memory the
  index held about 51 MiB, and now about 17 MiB at its peak (the engine's directory, and the
  blocks it still holds open: a history scanned newest first leaves most of them open).
- **The file is a cache, never the truth.** It is replaced whenever the index is built (the first
  query or `sync_recaps`, so at every start) and removed when the service is dropped: it is never
  read from one run to the next, so a crash, even in the middle of a write, can leave a file behind
  but never a stale answer. It is private (0600 on Unix). It is written without a journal on disk
  and without `fsync` (`journal_mode=MEMORY`, `synchronous=OFF`, an exclusive lock), since a crash
  discards it anyway; a transaction still rolls back. One file per service: two services (or
  processes) must not share a path.
- **A write that fails rebuilds the index.** If a batch of blocks cannot be stored (a full disk),
  the database is behind the engine: the index is broken, the query that met it answers an
  internal error, and the next one builds the index again from the log, with its blocks in memory
  from then on (logged). If the file cannot be made at all, the blocks stay in memory (a warning).
- **Memory.** One directory (not a second copy for names) keeps the recap index's own memory to
  about the engine's own figure for one (125–170 MB at the bound; see the `crates/recap` README),
  half what two would cost, plus SQLite's page cache for the file (1 MiB). In memory
  (`Recaps::new`), the blocks take one to six kilobytes each as JSON, depending on how much each
  holds (see "Timings"). A hub restart rebuilds the index.

**The seeded demo is not the fixture.** `crates/fixtures/data/demo-recaps.json` is the engine over
the demo's slice of events, with the demo's lists known beforehand; fed exactly that, the index
serves it, alone and through a store (`tests/recaps.rs`). A hub seeded with `seed` serves something
else, because its log is different: the seed's own events (`task_created`, `session_discovered`,
`dispatch_started`, `ask_raised`, `brief_accepted`, ...) are activity and make blocks of their own;
they are grouped by kind, each at its own time, before a slice that is partly older, so
out-of-order times split and join blocks (the engine closes a block on seeing an event more than
20 minutes later); and their ids are new ULIDs, so the blocks they begin sort first, in no stable
order among themselves. The test checks that the index serves what the engine makes of the seeded
log, that the fixture's blocks no seed event reaches come back unchanged, and that every other
block holds seed events.

## Dispatch

`POST /v1/tasks/{id}/dispatch` (`WorkService::dispatch_task`, `src/dispatch.rs`):

1. checks the request (`404` task, before the body is read; `400` agent, machine, a person as the
   agent, or a brief longer than `MAX_BRIEF`, 64 KiB, given or the task's description it defaults
   to: it goes on the CLI's command line; `403` an agent the caller does not own, since **a person
   runs only their own agents**, `require_owner`; `409` done or canceled, or the agent already
   holds an active dispatch on the task, such as a second click; `503` no live machine; in that
   order: the `403` needs the agent known), then whether a session can start there at all
   (`503` without a dispatcher, or when `Dispatcher::can_start` says no: the daemon's answers so
   when no runner is attached yet, or for a machine it cannot reach). Nothing is recorded for any
   of these. Then it appends, in one transaction: `task_assigned` to the agent if the task has
   none, `dispatch_started` (naming a new session id), and `session_discovered` for that session
   (state `starting`, `link_basis: dispatch`);
2. calls `Dispatcher::start` with a `DispatchRequest` (everything decided: machine, folder, engine,
   persona, model, permission mode, brief, the session id), without the command lock. Its
   `start_command()` is the runner's `StartSession` with `session` set to that id: the runner
   reports the CLI's transcript under it, never as a second session;
3. if that fails, logs why, appends `dispatch_finished` (outcome `failed`, the reason as the
   summary) and `session_ended`, and answers `503`, `409` or `500`. A panicking dispatcher counts
   as failed.

The machine is the request's, else the machine of the task's workstream's first location, else the
project's root's, else the hub's own. **The daemon must name the hub's machine** with
`with_hub_machine`, or with `set_hub_machine(&self, MachineId)` on the shared service once a
fresh hub is set up (its machine exists only then); without it, a dispatch with nowhere else to
run answers `503` (the hub does not guess one of the workspace's machines). The folder is the
first of those locations on that machine, else `~`. The engine, model and permission mode come
from the agent's persona (Claude Code by default).

**The task moves itself.** The runner link hands every batch of runner events the store accepted
to `WorkService::follow_sessions`:

- the dispatched session's first `working` (its re-stated `session_discovered` in that state, or
  a `session_state_changed` to it), or its first transcript `turn_ended`, calls `dispatch_working`:
  a completed turn is evidence of work even if the runner's first read folds state changes and
  discovers the session already idle. The task moves to in progress, as the
  agent for its owner. Once per dispatch while the service runs, so a person who moves the task
  back is not overruled by the agent's next turn (after a restart, the next `working` may move it
  once more, as the rules allow);
- the agent's report: when the dispatch's agent moves its task to review (`pitcrew report <task>
  --review`), `move_task` appends `dispatch_finished` (`succeeded`) for each active dispatch the
  agent holds on the task, in the same transaction as the move. A task a person already moved to
  review counts as the report: the dispatches succeed, nothing moves, and the move answers the
  task as it is (not `409`). The back office's `dispatch_to_review` rule moves a task still in
  progress when a dispatch succeeds;
- `session_ended` (or a change to `ended`) of a session whose dispatch is still open finishes it:
  `canceled` with "The session ended without a report." (the recaps say "stopped work"), or
  `failed` with "The session ended before its CLI started." when the runner never reported the
  session (its CLI id is still empty). Authored by the agent, for its owner.

Events about sessions without an open dispatch change nothing, so replays are harmless.

`reconciling_sessions(machine)` also lists sessions with active dispatches after their CLI id
appears. The runner link keeps watching their terminals. `dispatched_cli_exited(session)` ends
a reported session and cancels its active dispatch together, with the same summary and authorship
as an end hook. It rechecks under the command lock, so a report that already succeeded is kept;
ending the dispatch also releases the agent for another dispatch on the task.

**Sessions stored ahead of the runner** (a dispatch's, or `POST /v1/sessions` with `agent` or
`task`, recorded by `record_start`: `starting`, the agent named, linked to the task by hand) have
an empty CLI id until the runner reports them. `unreported_sessions(machine)` lists them, and the
runner link reconciles them, at start (a crash between step 1 and step 3) and while a start it
made is not confirmed: a session whose CLI the runner did start is reported under its id; one it
never started, or whose terminal ended before its transcript appeared, or (Codex, OpenCode) whose
transcript did not appear within the runner's 15-minute claim window, is abandoned
(`abandon_session`: its open dispatch finishes as `failed` with the reason, and it ends). A start
still under way (the runner link has not had the runner's answer) is never abandoned. A session
already ended or reported meanwhile is left as it is.

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
| `move_task` | person; agent on its own task | 404; 403 agent on another's task; 409 when `can_move` says no, or a racing writer moved the task first. An agent's move to review finishes its active dispatches on the task as `succeeded` (see "Dispatch") |
| `assign_task` | person | 404; 400 unknown member |
| `replace_subtasks` | person (whole list); agent on its own task (only its own `agent_plan` lines, in place) | 404; 403; 400 empty text, repeated ids, `agent_plan` naming a non-agent |
| `post_comment` | person; agent on its own task | 404; 403; 400 empty text, unknown mention |
| `raise_ask` | anyone; an agent only about its own task and session | 400 unknown task or session; 403; 400 empty title, unknown addressee |
| `answer_ask` | the addressee, or a person for their agents; decisions, approvals and reviews only by people | 404; 403; 400 no option and no non-blank text, option out of range; 409 already answered |
| `put_brief` | person | 404 unknown target. With the pending proposal's text and next, `brief_accepted` carries its receipts (and the brief is the back office's) |
| `patch_workstream` | person | 404; 400 empty patch, or links that break `links::check_links` (over 16, repeated, a key with control or hidden characters, a URL that is not `https://` or has a user name). `external` appends `workstream_linked` with the full new list (after `workstream_changed` when status or health changed too) |
| `dispatch_task` | person | see "Dispatch" |
| `dispatch_working` | the runner link (through `follow_sessions`) | moves the dispatched task to in progress as the agent, when the rules allow; 409 for an ended dispatch, or a racing writer that moved the task first |
| `follow_sessions` | the runner link | see "Dispatch": a dispatched session's first `working` moves its task, its end finishes its dispatch |
| `record_start` | person (`POST /v1/sessions` with `agent` or `task`) | 403 agent; 400 unknown machine, agent or task, or a person as the agent; 403 an agent the caller does not own. Appends the `starting` session |
| `abandon_session` | the runner link | 404 unknown session; nothing for one ended or reported. Fails its open dispatch and ends it |
| `mirror_plan` | not called yet | replaces the agent's `agent_plan` lines of its own task from a `PlanUpdated`; the runner reports no plans to the hub yet |
| `seed` | the daemon | imports a `DemoWorkspace` into an empty work model |
| `ensure_office_member(owner)` | the daemon | finds or adds the back office's member (see "The back office"); 400 `owner` is not a person; 409 `@office` held by a person, another person's agent or no one's agent |
| `run_office`, `OfficeCommands` | the back office | see "The back office" |
| `ensure_sync_member(owner)` | the daemon, when a person connects a tracker (each connection keeps its own) | finds or adds the tracker sync's member: an agent of `owner` named `@sync` (`@tracker-sync` when `@sync` is someone else's); 400 `owner` is not a person; 409 both handles taken |
| `SyncCommands` | a tracker sync (`sync_commands(member)`) | see "Tracker sync" and "Outward writes" |

"Own task" means the agent is the assignee or holds an active (not ended) dispatch on it.

Every `400` comes before a `409`: a request that is malformed and would also conflict is a `400`.
Object bodies must be JSON objects (serde would otherwise read an array as a struct's fields by
position); `PUT /v1/tasks/{id}/subtasks` takes an array. A key given twice in a body counts once,
with its last value, as `JSON.parse` does in the mock hub.

Lists in a body (up to 1 MiB) cost time linear in their length: the checks that need no table run
before the command lock, labels in one pass that stops at the 33rd distinct one, repeats dropped
with a hash set; the checks against the tables take one query per list (`json_each`), and a
`blocked_by` cycle is one recursive query over the tasks waiting on the edited one.

## Tracker sync (`SyncCommands`)

A GitHub or Jira sync (`pitcrewd`'s `integrations`, api-v1.md "Integrations") changes the work
model only through `SyncCommands`, as its own member (`@sync`, an agent of the person who connected
the tracker), each command re-checked under the command lock like any caller's:

- `create_task(workstream, source, title, description, labels)`: status `todo`, the upstream item
  as `source`, text made to fit (`fit_title`, `fit_labels`: hidden characters dropped, 1–500
  characters, labels trimmed and cut to 64 characters, at most 32). A source already mirrored
  returns that task and appends nothing.
- `update_task(task, title?, description?, labels?, workstream?)`: upstream-owned fields and the
  workstream (of the task's project); `task_updated` with what differs, or nothing.
- `move_task(task, to)`: `can_move(.., Mover::Sync)` from the status the task is in now: in-progress
  work is never touched (`Refused`), and a move to where the task already is appends nothing.
- `set_workstream_status(workstream, shipped)`: refused while one of its tasks is in progress.
- `note(task, text)` (a merged pull request) and `raise_conflict(task?, title, body)` (a `decision`
  ask to the owner): never posted or raised twice while the same one stands.
- `task_by_source`, `workstreams`, `work_in_progress`: what routing and planning read.

`links::scope_of(link)` says what a workstream link names (a repository or milestone, a Jira
project or epic); the sync routes issues by it. Nothing here knows GitHub or Jira: the daemon reads
them and plans with `pitcrew-sync-github` and `pitcrew-sync-jira`.

## Outward writes (`writes.rs`, `work.writes`)

A write to GitHub or Jira (api-v1.md, "Outward writes") is approved by a person first; these
commands are the gate the daemon sends through:

- `propose_write(to, write, title, body)`: an `approval` ask from the sync's member (each
  integration's own) to `to` (a person), with the task, `["Send", "Don't send"]` and the cause as
  a receipt, and `write_proposed`, in one append; `None` when a write was already proposed for that
  cause.
- `WorkService::request_retry(caller, ask)`: `write_retry_requested { ask, task, by }`, authored
  by a person who may answer the write's ask, for a `failed` write; a second request while one
  waits appends nothing.
- `start_write(ask)`: `write_started`, only for a write whose ask is this member's own approval ask
  answered "Send" by a person, or a failed one with a person's retry request no attempt has used
  yet (the start uses it). Anything else is `Refused`.
- `finish_write(ask, result)`: `write_finished`; `sent` and `failed` only after a start, `not_sent`
  only before one (or after a failure).
- `writes(filter)`, `write(ask)`, `check_retry(caller, ask)` (who may answer its ask; `failed`
  only) are what the routes read; `UpstreamWrite.retry_requested_by` says whose retry waits.

The `work.writes` projection (migrations `0401_work_writes.sql` and `0402_work_write_retries.sql`)
applies the same transitions from the log, so a rebuild gives the same rows. A `write_finished` that created an issue gives its task
that issue as `source` (`work.tasks` v4), and write events are their task's activity (`work.refs`
v3).

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
| `AskAnswered` | an open question or mention addressed to the office itself, as api-v1 lets any agent answer; the answer is the office's (`by`, `at`) | 403 an ask to anyone else, a decision, approval or review; 404; 400 bad option or no answer; 409 answered |
| `CommentPosted` | on a known task or workstream (when it names both, the task is in that workstream), known mentions, text not blank | 404; 400 |
| any other event | never | 403 |
| `raise_ask` | a known addressee, task and session; not an approval | 403 approval; 400 |
| `propose_brief` | a known target: `brief_proposed`, and with it `accepted_body()` when the proposal says auto-accept, unless the brief in force is pinned (then only proposed) | 404 |

Every action must cite receipts (400 without). The office's member must be an agent; a person
cannot be the back office.

**Its member** (`ensure_office_member(owner) -> Member`): the member holding `@office`
(`OFFICE_HANDLE`) when it is an agent owned by `owner`, the workspace's person; when no member
holds the handle, a new agent ("Back office", owned by `owner`) appended in one `member_added`
authored by `owner`, under the command lock like any command, so the daemon may call it while the
hub serves (right after `set_up`) as well as at start. `@office` held by a person, another
person's agent or no one's agent is a `409`: the back office never acts as a person, nor for
someone else. Found or added once and reused, since the run log's settings name the member.

**Running a range again is safe.** The events an action appends get ids derived from the store's
`log_id`, the run-log entry's revision and `seq`, and the event's place in the action (a ULID with
the entry's time and 80 bits of a SHA-256 of those), and are appended with `append_new`. An action
whose first event is already in the log is reported as `replayed` and appends nothing. So:
- after a restart, the daemon may re-run the range it is not sure it finished (e.g. from the last
  revision it recorded as done): what was applied is skipped, what was not is applied now. A crash
  between an append and its `run_office` loses nothing if the daemon re-runs that range;
- re-run recent ranges only: an action the hub refused then is tried again, against the state now.

**What the daemon must know.**
- Read `last` right after `subscribe()` and before anything appends, and run each range from
  `last + 1`, as in "Wiring": a batch appended between the two would otherwise be missed, and a
  batch appended by another process is never announced.
- An action the office emitted and the hub refused (the office's view was out of date, e.g. after
  a racing writer's stale move) is `emitted` in `office_runs`: a projection cannot record what the
  hub later did. `run_office` returns and logs the refusal.
- The store's run log and the `BackOffice` must have the same rules: `run_office` fails when a
  revision has more entries than the office's rules can make.

## Routes

Agent and device tokens (`agent_routes`): `GET /v1/me`, `GET /v1/members`,
`GET /v1/tasks?project=&workstream=&assignee=&status=`, `GET /v1/tasks/{id-or-key}`,
`POST /v1/tasks/{id}/move`, `PUT /v1/tasks/{id}/subtasks`, `POST /v1/tasks/{id}/comments`,
`GET /v1/asks?to=&state=`, `POST /v1/asks`, `POST /v1/asks/{id}/answer`.

Device tokens only (`device_routes`): `GET /v1/workspace`, `POST /v1/setup`, `GET /v1/machines`,
`GET /v1/personas`, `GET /v1/teams`, `GET|POST /v1/projects`, `GET /v1/projects/{id}`,
`GET /v1/workstreams?project=`, `POST /v1/workstreams`, `GET|PATCH /v1/workstreams/{id}`,
`POST /v1/tasks`, `PATCH /v1/tasks/{id-or-key}`, `POST /v1/tasks/{id}/assign`,
`POST /v1/tasks/{id}/dispatch`, `GET /v1/sessions?machine=&workstream=&task=&state=`,
`GET /v1/sessions/{id}`, `POST /v1/sessions/{id}/link`, `GET /v1/briefs`, `PUT /v1/briefs/{project|workstream}/{id}`.

`GET /v1/workspace` answers `{ workspace, rev, setup_needed }`; `rev` is the lowest checkpoint of
the work projections (`projection_state.rev`), the revision every work table reflects, and
`setup_needed` is `true` while the workspace has no person yet, omitted (never `false` on the
wire) once it does. See "The first run" for `POST /v1/setup` and the daemon's seam.

## The first run (`POST /v1/setup`)

A fresh hub has a device token but no person, no machine and no name (api-v1.md, "The first
run"). `WorkService::set_up(caller, Setup) -> Result<SetupDone>` (`src/setup.rs`) runs it once,
through the one writer:

- device tokens only (`forbidden` for an agent, checked before the body);
- every field checked exactly as the contract says (the three names trimmed, then counted in
  code points and stored trimmed; the handle's shape, not trimmed; no control characters), `400`
  before the `409`s. Whitespace is what JavaScript's `trim` removes (Unicode's `White_Space`
  except U+0085, plus U+FEFF), so this hub and the mock hub store the same names;
- `409 conflict` once the workspace already has a person, or if the handle is taken (by another
  member the hub already knows, e.g. a back office added before a person existed) — so a retried
  request never makes a second person: the first call to commit wins, every other sees the
  conflict. `@office` (`OFFICE_HANDLE`) is reserved for the back office and always taken, even
  before its member exists;
- appends, in one append, `member_added` for the caller's own member id (kind `human`, no owner)
  and `machine_added` (kind `local`, liveness `live`, a new id), both authored by the caller;
- returns the new member and machine alongside the workspace (its id, and the name just set).

**Two seams for the daemon**, since the workspace's name is kept outside the event log (in
`workspace.json`, which the daemon owns) and setup is also when the daemon must start what needed
a person (the back office, and the runner on this machine; api-v1.md, "The first run"):

- `WorkService::set_workspace_name(&self, name: String)`: sets the name `GET /v1/workspace` (and
  `set_up`'s own `SetupDone.workspace.name`) serves from then on. `set_up` calls it itself once its
  append commits, with the name the request gave; the daemon also calls it on every start, with
  the name it read from `workspace.json`, so the two never disagree. Without a call, the service
  serves the name it was constructed with (`WorkService::new`'s `workspace.name`).
- `SetupListener` (`pub trait { fn set_up(&self, done: &SetupDone); }`), registered with
  `WorkService::with_setup_listener(Arc<dyn SetupListener>)` before the service is shared. Called
  **once**, synchronously, right after `set_up`'s append commits (so it never sees a setup that
  did not take) and with the command lock still held (so nothing else can write to the work model
  between the commit and the listener seeing it). The daemon's listener is where `workspace.json`
  gets written and the back office and runner get started; this crate does neither (out of scope;
  see the brief).
  - **Reads only, never a write, and never panic.** The lock still held is a plain, non-reentrant
    `std::sync::Mutex` (`set_up` itself is still on the stack): reads (`WorkService::read`,
    `members`, `tasks`, ...) are fine, but calling any command (`create_task`, a second `set_up`,
    ...) from the listener deadlocks the thread. A panic here unwinds through `set_up`: the route
    answers `500`, but the append already committed, so a retry then answers `409`, never a second
    person. If the daemon's own work can fail or needs to write to the work model, hand `done` to
    its own task (a channel, `tokio::spawn`, ...) and do that work off this call stack.

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
  briefs, through the routes (the cases of the mock hub's `edits.test.ts`), project key clashes
  from a racing writer, repeated JSON keys, and lists as long as a 1 MiB body allows.
- `tests/office.rs`: a finished dispatch moves its task to review; actions the office's guard
  refuses are logged `refused` and refused by the hub too; an action emitted on an out-of-date
  view is refused by the hub; every (from, to, mover) through `OfficeCommands` against
  `can_move`; answers (only the office's own asks), asks, comments and brief proposals (pinned
  and not); a run log several pages long applied whole, once, in order; a range run twice
  appends once, and a re-run after a crash applies only what was missing; a run log with other
  rules than the `BackOffice` is an error; `ensure_office_member` adds `@office` once, by the
  person, finds it afterwards (and the demo's), and is a `409` when someone else holds the handle.
- `tests/rebuild.rs`: rebuilding every projection, building them on open, and applying one event
  per append all give identical tables, over every table, with task edits, key clashes, new
  projects and workstreams, and pending and accepted proposals in the log.
- `tests/single_writer.rs`: key clashes, stale moves, catch-up on open, concurrent `create_task`,
  a racing writer's `409`, keys by prefix, `on_behalf_of`, internal errors.
- `tests/rules.rs`: the whole move-rules table through the routes, agent scope (every device route,
  mounted bare and guarded), authorization before validation, who answers asks.
- `tests/refs.rs`: the reference index against an independent oracle over the demo, paging,
  the scan budget, links over time, the query plans.
- `tests/recaps.rs`: the recap index serves `demo-recaps.json` when fed the fixture's events (in
  memory, and through a store whose log holds them after what the fixture knew), and why the
  seeded demo differs; every page, filter and day of a generated log through the hub against the
  engine over the whole log at once, at page sizes from 1 to over the cap; filters combined,
  `before`, defaults and caps; odd and hostile input; a growing open block; the day cache (a
  repeated query writes nothing, a growing block rewrites only its day, a rename rewrites all, a
  new task or ask nothing named rewrites none, a name learned late rewrites the paragraph that
  named it as a rebuild does, the bound); an eviction for the directory's own bound rewrites the
  days that named the entry it dropped; refused task creations, and a tasks projection lagging
  behind another process's appends; appends from another connection; receipts; shared use from
  several threads. A unit test in `src/recap.rs` covers an event the engine panics on.
  `tests/recap_props.rs`: property tests that an index kept current through random batches, with
  queries in between, equals a rebuild and the engine over the whole log, through a store and in
  memory (with small engine caps and caches, and with the directory's own bound tiny, so eviction
  starts from the first events on); and every receipt points into the log. `tests/recap_file.rs`:
  the same with the blocks in a file (`with_recap_file`), which equals a rebuild in memory and the
  engine, lives and dies with the service, and when a crashed run left one behind (another log's
  index, or garbage) is replaced, never served. `src/recap_db.rs` unit tests cover local placement,
  private local fallback, memory fallback, and safe reuse of a folder left by a hard kill.
  Unit tests in `src/recap.rs`: a batch of blocks
  that cannot be stored fails its query, and the next one rebuilds from the log. `tests/recap_common/`
  holds the generator and the oracle.
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

**The recap index**: `cargo test -p pitcrew-hub-work --release --test recap_perf -- --ignored
--nocapture` builds a log of 100,000 generated events, then times a fresh service's first
`sync_recaps` (the rebuild) and the queries. Measured 2026-10-01 on the same laptop, release build,
under load (load average about 10 on 14 threads: other agents were compiling):

| 100,000 events | Blocks | Rebuild, best / median of 5 | Reading the log alone | The engine alone |
|---|---|---|---|---|
| sparse: 200 sessions, events up to 2 min apart | 62,058 (72.5 MB as JSON, lines included) | 1,022 / 1,281 ms | 599 ms | 182 ms |
| bursty: 24 sessions, bursts seconds apart | 2,819 (16.7 MB) | 328 / 566 ms | 284 ms | 37 ms |

Queries, median of 21 (sparse / bursty): the newest 50 blocks 0.15 / 0.49 ms; a session's 200
blocks 1.3 / 1.3 ms; a project's 30 days from the cache 4.5 / 0.45 ms (cold: 39 / 17 ms); a page
right after one new event 1.6 / 0.9 ms. Reading and decoding the log is most of a rebuild.

## Differences from the mock hub

- The mock accepts the display form of an id in a body (`"project": "prj_01JB…"` in
  `POST /v1/workstreams`). api-v1 says ids are bare ULIDs, and the hub answers `400` for any
  other form in a body (paths take both).
- The mock moves a dispatched task to in progress when its simulated session starts working, as
  the hub does, but does not finish a dispatch: not when its agent moves the task to review
  (`succeeded`), nor when its session ends (`canceled` or `failed`), as api-v1's "Dispatch" now
  says the hub does (`follow_sessions`, `move_task`).
- The mock lets a person dispatch any agent; the hub only the caller's own (`403` otherwise), and
  refuses a brief over 64 KiB (`400`). `apps/mock-hub` is outside this crate's paths.

Manual links are person-only (`WorkService::link_session`): an unknown session is 404; missing
or inconsistent task/workstream references are 400. A task-only link derives its workstream; a
workstream-only link clears the task. Each emits `session_linked` with basis `manual`. Imported
links are firm, matching the runner; the sessions projection version is bumped so replay applies
that rule. `tests/manual_links.rs` covers route validation and protection against later inference.

## Directory writes and creation dialogs

Device-only `POST /v1/personas`, `PUT /v1/personas/{id}`, `POST /v1/teams` and
`PUT /v1/teams/{id}` validate all fields/references before appending `persona_saved` or
`team_saved`. `PersonaEdit` and `TeamEdit` are protocol request types. Creating a persona also
appends its caller-owned agent member in the same batch; editing updates linked member names.
Teams retain their existing member-id model, deduplicate members and include the lead.
`NewProject.first_workstream` optionally creates the project's first workstream in the same
append, using the project root as its location. This keeps the creation dialog atomic.
`tests/directory.rs` covers refusals, authorization, atomic creation and directory replay.

Successful onboarding scans call `ensure_engine_agents`: missing detected engines receive a
person-owned agent member and default persona in one batch. The writer lock makes concurrent
scans idempotent; existing owned agents are reused. Service actors without personas do not count.

Workspace safety preferences are projected from person-authored `safety_changed` events (`work.safety`), with validated permission modes and a 0–100 hourly automatic-acceptance budget. An explicit save controls low-risk task completion and brief acceptance; disabled or exhausted budgets leave proposals for review. Existing hubs retain their per-task policy until the first explicit save. Rebuilding preferences retains the hourly budget because acceptances remain in the event log.

Onboarding review: hook previews detect supported CLIs on PATH or through their
homes, skip conflicting engines while applying other changes, and report the
skipped engines. No-change previews cannot set the wizard's installed flag.
Desktop packages include the hook CLI beside the daemon. Safety uses snake_case
wire fields and the shared PermissionMode enum; bypass defaults are currently
refused. Unsaved safety reports `saved: false` for legacy per-task acceptance.

Directory creation uses friendly engine handles, ownership-checked persona edits, and runner-safe model/permission values. Dispatch lists and accepts only persona-linked agents. Every successful scan provisions missing owned agents idempotently. Local setup records OS/architecture and local project roots must be absolute on that platform. Creation dialogs focus Name; tasks default to the current project and filter its workstreams.
