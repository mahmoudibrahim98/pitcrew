# Stream E · Hub: work model

**Goal:** projects, workstreams, tasks and subtasks, asks, dispatch and queue: the commands that
change them, the projections that show them, and the rules that make tasks move themselves.

**Owns:** `crates/hub-work/**`, `crates/store/migrations/02*`.  **Depends on:** stream 0, C.
**Model:** Opus-class.
**Read first:** ADR-0007, ADR-0004, ADR-0006; `crates/protocol/src/{model,events}.rs`;
`docs/build/contracts/api-v1.md` (projects, tasks, sessions linking, asks, briefs, events).

## Work packages

1. **Tables and projections** (migrations 0200–0299): projects, workstreams, locations, tasks
   (keys allocated per project), subtasks, task dependencies, labels, dispatches, queue, asks,
   comments, briefs, members, personas, teams. All built from events via C's projection trait.
2. **Commands** with validation, each appending events stamped from `Caller`: create and edit
   project / workstream / task; move (`TaskStatus::can_move` with the right `Mover`); assign;
   replace subtasks; comment with mentions; raise and answer asks (decisions and approvals need a
   person); edit and pin briefs.
3. **Tasks move themselves:** a dispatch moves its task to in progress; an agent's report moves
   it to review; the agent's live plan (`PlanUpdated` items from the runner) replaces the task's
   `agent_plan` subtasks. People can always override.
4. **Dispatch and queue:** start a session for an agent on a task (a `StartSession` command to
   the right runner), per-agent concurrency, a queue, hand-off.
5. **Routes** (`routes()`, see `contracts.md`) for every work route in API v1, with the query
   filters listed there.

## Acceptance

- The fixture's event slice, replayed, produces the fixture's tasks, asks and briefs.
- The move-rules table from the protocol is enforced end to end (agent on another's task → 409).
- An agent token cannot answer a decision or approval (403).
- Plan mirroring: a plan update from the agent's own session replaces only `agent_plan`
  subtasks, never `human` ones.
- Listing 10,000 tasks with a filter in < 20 ms.

## Do not

Touch the store's core (C), write recaps or briefs proposals (F), or call GitHub/Jira (G).
