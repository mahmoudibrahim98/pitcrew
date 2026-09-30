# Brief E · Work model core

- **Stream:** E · Hub: work model. **Branch:** `s/E/work-core` (based on `s/C/projections`, the
  store's projection API, until that merges). **Paths:** `crates/hub-work/**` and
  `crates/store/migrations/02*`.
- **First read:** [README.md](README.md), then `docs/build/streams/E.md`, ADR-0004, ADR-0006,
  ADR-0007, `crates/protocol/src/{model,events,api}.rs` (note the new `MachineAdded`,
  `MemberAdded`, `PersonaSaved`, `TeamSaved` and `SessionUpdated` events, and `Caller`),
  `docs/build/contracts/api-v1.md`, and `crates/store` (its README: the `Projection` trait,
  `pitcrew_store::sql`, `Store::read`, the migration rules).

## Goal

The hub's model of the work, built from the event log:
- **projections** into queryable tables;
- **commands** that validate and append events (stamped from the `Caller`);
- **axum routes** for the work parts of API v1.

## What to build

1. **Migrations** `0200_…` onwards, all `STRICT` and following the store README's rules:
   - members, machines, personas, teams;
   - projects, workstreams (with locations), tasks (keys allocated per project), subtasks,
     task dependencies, labels;
   - sessions (as seen by the hub), dispatches;
   - asks, comments, briefs.
2. **Projections** (the `Projection` trait) from events to those tables. They are
   **idempotent** with respect to rebuilds; apply order is revision order. **Rules** (from the
   store review; they keep incremental application and rebuild identical):
   - `apply` has no side effects: it doesn't append events and doesn't read the clock;
   - `apply` never reads **another** projection's tables. Keep each projection
     self-contained, and do joins in `Store::read`;
   - no foreign keys between different projections' tables;
   - use `event.author` and `on_behalf_of` from the `StoredEvent`, never the live caller;
   - bump `version()` with any migration that reshapes a projection's tables.
   - Derived events (e.g. "a dispatch started, so move the task") belong in **commands**,
     which append them, never inside `apply`.
3. **Commands** (`WorkService` or similar), each validating against the current tables and
   appending events through `Store::append`:
   - create task (next key per project);
   - move (`TaskStatus::can_move` with the right `Mover`: a person for device tokens; an agent
     with `on_own_task` computed from assignee or active dispatch);
   - assign;
   - replace subtasks (an agent replaces only its own `agent_plan` lines);
   - comment with mentions;
   - raise and answer asks, with the rules in `api-v1.md`: decisions, approvals and reviews
     need a person; answering is limited to the addressee or its owner;
   - put a brief;
   - patch a workstream.

   Also a **seed** command that imports a `DemoWorkspace` (from `pitcrew-fixtures`) as events,
   so the real daemon can serve the demo data.
4. **Routes** (`routes()` returning an axum `Router`, per `docs/build/contracts.md`), reading
   the `Caller` from `Extension<Caller>`:
   - `GET/POST` for tasks, projects, workstreams, members, machines, personas, teams, asks and
     briefs;
   - move, assign, subtasks, comments;
   - errors as `ApiError` with `ErrorCode::http_status()`;
   - agent-scope rules per `api-v1.md` (reads are workspace-wide; writes only on the agent's
     own tasks).

   Split routes into an agent-allowed router and a device-only router, so the API layer can
   mount them with `RouterParts::agent` and `RouterParts::device`.
5. **Agents move their own tasks and mirror plans.** A `PlanUpdated` from the agent's own
   session (and the runner's future `subtasks` events) replaces the task's `agent_plan`
   subtasks. A dispatch start moves the task to in progress. Keep this as a command the runner
   or hub link can call; it doesn't need a route yet.

## Acceptance

- **Seed:** after seeding the demo workspace, the routes return the same data as the mock hub
  for the demo, compared structurally (ids, keys, fields).
- **Rebuild:** dropping and rebuilding every projection from the log gives identical tables.
- **The move-rules table**, end to end through the routes: an agent on another's task gets 403;
  a rule violation gets 409; a person can make any legal move.
- An agent token can't answer a decision or approval (403); an agent's `PUT subtasks` keeps
  human lines.
- Listing 10,000 tasks with a filter: under 20 ms (report the number).

## Out of scope

Dispatch execution (starting sessions on runners), the queue, rooms and messages, and GitHub
and Jira (stream G).
