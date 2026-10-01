# Brief 0 · Editing tasks, creating projects and workstreams, brief next steps

- **Stream:** 0 · Contracts (run as integration work). **Branch:** `integrator/work-edits`.
  **Paths:** `crates/protocol/**`, `docs/build/contracts/**`, `apps/mock-hub/**`, plus the
  smallest compile or test fixes other crates need for the new event variant (for example the
  every-variant samples in `crates/store/tests/event_log.rs`). Nothing else.
- **First read:** [README.md](README.md), `docs/build/contracts.md`,
  `docs/build/contracts/api-v1.md`, `crates/protocol/src/{model,events,api}.rs`, and
  `apps/mock-hub/src/routes.ts` with its tests.

## Why

Stream E (the hub's work model) and stream N (the Projects UI) found gaps in API v1:

- nothing edits a task's title, description, priority, labels, dates, dependencies or
  workstream;
- api-v1 has no route to create a project or a workstream (onboarding needs both);
- `brief_accepted` and `brief_proposed` can't carry the brief's next step, although
  `PUT /v1/briefs` accepts `next`.

All changes are **additive**: events already in a log must still decode.

## What to build

1. **`TaskPatch`** (`crates/protocol/src/model.rs`), a partial update of a `Task`:
   - Plain fields: `title`, `description`, `priority`, `labels`, `blocked_by`, `accept_auto`.
   - Nullable fields: `start`, `due`, `workstream`, as `Option<Option<T>>`. A missing field
     means unchanged, JSON `null` means clear, and a value means set.
   - Serde: every field `default` and skipped when `None`. Write a small private helper module
     for the nullable fields; add no new dependency.
   - Methods: `is_empty()`, and `apply(&self, task: &mut Task)`.
   - Tests: missing, `null` and value each round-trip exactly; `apply` works; an empty patch
     serializes as `{}`.
2. **Events** (`crates/protocol/src/events.rs`):
   - `TaskUpdated { task: TaskId, patch: TaskPatch }`. The patch holds only the fields that
     actually changed.
   - `BriefProposed` gains `next: Option<String>` (default, skipped when `None`).
   - `BriefAccepted` gains `next: Option<String>` and `receipts: Vec<Receipt>` (default,
     skipped when empty).
   - Test that the old JSON of both brief events, without the new fields, still decodes.
3. **Request types** (`crates/protocol/src/api.rs`), so the hub, CLI and mock share them:
   - `NewTask`, exactly as api-v1 describes it today;
   - `NewProject { key: ProjectKey, name, lead?, members?, status?, start?, due?, root? }`;
   - `NewWorkstream { project, name, status?, locations? }`.
   - Tests: JSON round trip, including defaults.
4. **Contract** (`docs/build/contracts/api-v1.md`):
   - `POST /v1/projects`, `NewProject` → `Project` (201). **Device tokens only.**
     - Defaults: `lead` is the caller's member, `status` is `in_progress`.
     - `409 conflict` when the key is already used.
     - The key uses the `ProjectKey` format; check it in `model.rs`, and say it in the doc.
     - Emits `project_created`.
   - `POST /v1/workstreams`, `NewWorkstream` → `Workstream` (201). Device only.
     - Defaults: `status` is `active`, `health` is `on_track`.
     - `404` for an unknown project.
     - Emits `workstream_created`.
   - `PATCH /v1/tasks/{id-or-key}`, `TaskPatch` → `Task`. Device only.
     - Emits `task_updated` with only the fields that changed. A patch that changes nothing
       returns the task and emits nothing.
     - Rules, each with its error code:
       - `title` is 1 to 500 characters after trimming;
       - labels are trimmed and deduplicated, each 1 to 64 characters, at most 32;
       - `workstream` must belong to the task's project;
       - `blocked_by` holds existing tasks, never the task itself, and creates no cycle
         (`409 conflict`);
       - dates are `YYYY-MM-DD`, and `start` ≤ `due` when both are set.
   - Briefs: `PUT` stores `next`, and `brief_accepted` carries it.
     - When a person accepts the pending proposal unchanged (same text and next), the hub
       copies the proposal's receipts into `brief_accepted`, and `source` is `back_office`.
     - Define the pending proposal: the newest `brief_proposed` for that target, if it is newer
       (higher `rev`) than the brief in force.
     - "Keep current" is a `PUT` of the current text. It needs no new event, because the
       accepted brief is then newer than the proposal.
   - Add each new route to the route table with **agent** / device marked, as the doc does now.
5. **Mock hub** (`apps/mock-hub`): implement the three routes and the brief `next` and receipts
   behaviour, following the contract exactly, with tests in its suite. Keep the demo data
   unchanged.
6. **Other crates:** add `task_updated` (and the new brief fields) wherever an every-variant test
   or an exhaustive `match` needs it. Change nothing else.

## Acceptance

- `cargo fmt`, clippy `-D warnings`, and `cargo test --workspace` all pass.
- `npm test` passes, including new mock-hub tests for every rule and error code above.
- `node scripts/ci/path-guard.mjs --base main`, and the scrub gate.
- Old events decode (tests).
- The contract doc and the mock agree: test each route through the mock's HTTP tests.

## Out of scope

- Hub-side implementation. Stream E does that in its next brief.
- UI changes (streams L and N).
- Per-subtask edits and `If-Match`.
