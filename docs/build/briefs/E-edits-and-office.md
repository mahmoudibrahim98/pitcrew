# Brief E · Task edits, project and workstream creation, brief proposals, and the back office

- **Stream:** E · Hub: work model. **Branch:** `s/E/edits-and-office`. **Paths:**
  `crates/hub-work/**`, `crates/store/migrations/02*` (+ `Cargo.lock`).
- **First read:** [README.md](README.md), then
  [E-single-writer-and-sessions.md](E-single-writer-and-sessions.md) and
  [0-work-edits.md](0-work-edits.md) (both merged). Also `docs/build/contracts/api-v1.md`
  (`PATCH /v1/tasks`, `POST /v1/projects`, `POST /v1/workstreams`, briefs and the pending
  proposal) and `crates/office` (the `Commands` trait, `apply()` and `RunLog`).

## Goal

The real hub does what the merged contract describes. The mock hub already does; its tests and
`apps/mock-hub/src/routes.ts` are the behavioural reference, but the contract document wins.
Then the back office can act on the hub.

## What to build

1. **`PATCH /v1/tasks/{id-or-key}`:**
   - `TaskPatch`, with every rule and error code in api-v1. The whole patch is checked before
     anything changes; a `blocked_by` cycle is a 409; a patch that changes nothing returns the
     task and emits nothing.
   - `task_updated` carries only the fields that changed.
   - The tasks projection applies `task_updated`. Bump `Tasks::VERSION`, since the task shape
     test won't force it.
   - Device tokens only.
2. **`POST /v1/projects` and `POST /v1/workstreams`**, with their defaults and errors, emitting
   `project_created` and `workstream_created`.
   - Project keys become unique; a taken key is a 409. Add a new migration if a unique index
     needs one; never edit a merged migration.
3. **Briefs:**
   - The projection reads `next` and `receipts` from `brief_accepted`.
   - It keeps the pending proposal: the newest `brief_proposed` with a higher `rev` than the
     brief in force.
   - `GET /v1/briefs` returns `proposal`.
   - A `PUT` with the same text **and** next as the pending proposal copies its receipts, and the
     source is `back_office`. Any other `PUT` is `person`.
   - Remove the parity test's exclusions for `next` and `proposal`.
4. **The back office:**
   - Implement `pitcrew_office::Commands` over `WorkService`, authored by the back-office member.
   - It **re-validates every action like any caller**: `can_move` with the right `Mover`,
     `accept_auto` before a move to done, and who the answered ask is addressed to. `apply()`
     only re-checks shape.
   - `propose_brief` appends `brief_proposed`, plus `accepted_body()` when the office's policy
     says auto-accept.
   - Register `RunLog` with the same `Config` the live office uses (`Config::office` = the
     back-office member).
   - Provide one entry point the daemon calls after each append batch, e.g.
     `WorkService::run_office(&Office, events)`. Document the wiring.
5. **Mock parity:** dump the mock's answers for the new routes and compare them, as the existing
   parity test does.

## Acceptance

- Every rule and error code of the three routes, through the routes, compared with the mock
  where it applies.
- Rebuild equals incremental over every table, including `task_updated` and briefs with
  proposals.
- The office through `Commands`:
  - a finished dispatch moves its task to review;
  - an action the hub refuses is refused, and logged in the run log as refused;
  - nothing bypasses `can_move`.
- The 10k-task list timings still meet the target.

## Out of scope

Daemon wiring (stream 0 does it from your documented entry point), the UI, and GitHub or Jira.
