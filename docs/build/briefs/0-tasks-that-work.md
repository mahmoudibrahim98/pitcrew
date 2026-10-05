# Brief 0 · Tasks that work

- **Stream:** 0 · Composition root (work model, projects UI).
  **Branch:** `integrator/tasks-that-work`.
  **Paths:** `crates/hub-work/**`, `crates/daemon/**` (dispatch status only),
  `crates/protocol/**` (regenerate `packages/protocol-ts`), `apps/ui/src/projects/**`,
  `apps/ui/src/design/**` (a toast and a side-drawer primitive), `apps/ui/src/shell/**` (only to
  mount the toaster), `apps/ui/src/data/**`, `apps/mock-hub/**`, `tests/conformance/**`,
  `docs/build/contracts/api-v1.md`, and the READMEs of what you touch. Mechanical edits elsewhere
  are fine; say which in the report.
- **First read:**
  - [README.md](README.md) and the root `AGENTS.md` (or `CLAUDE.md`);
  - `api-v1.md` (tasks, dispatch, events);
  - `apps/ui/src/projects/` (the task page, board, new-task dialog, My tasks);
  - [0-create-dialogs.md](0-create-dialogs.md): its PR #60 adds dispatchable agents. Build on it
    once it merges, and don't duplicate it.
- **Suggested agent:** Codex.

## Goal

The audit found that a created task is almost read-only:

- only status and assignee can be changed;
- there is no description, yet dispatch says it "defaults to the task's description";
- the New task dialog offers other projects' workstreams;
- nothing confirms a create;
- a dispatch whose CLI fails stays "Starting" while the log already says it failed.

## What to build

1. **Edit everything a task has:**
   - title, description (Markdown, rendered as text-safe HTML with no raw HTML), priority, due
     date, start date and labels;
   - dependencies (add and remove `blocked_by`, refusing cycles as the API does);
   - delete or archive, with an event, an undo in the toast, and hidden from boards.
   Add events or fields only where missing; stored types stay forward-compatible (no
   `deny_unknown_fields`, snake_case).
2. **A task drawer.**
   - Opening a task from a board, list, calendar or search shows a side drawer: properties with
     icons, description, subtasks with progress, agent run, dependencies, comments and activity.
   - "Open full page", "Mark complete" and "Copy link" are in the drawer; Esc closes it.
   - The full page stays at its URL.
3. **The New task dialog.**
   - Description, priority and labels.
   - Workstreams filtered by the chosen project.
   - Defaults from where it was opened: project, workstream, column status.
4. **Feedback.** A toast after create, edit, move and delete ("API-1 created · Open"), with errors
   that say what to do.
5. **Dispatch status.**
   - The task's agent run shows the live state and, when a run fails, the reason from
     `dispatch_finished`.
   - Nothing stays "Starting" once the hub has recorded the end: invalidate on `dispatch_finished`
     and `session_ended`.
   - "Open terminal" appears only when the session has one.
6. **My tasks** gets a list view grouped Overdue / Today / Upcoming / No date / Completed, with the
   board as the alternative view, remembered per person.
7. **Tests:**
   - every edit against the real hub and the mock;
   - the drawer's keyboard and focus;
   - dispatch failure shown on the task;
   - conformance for new routes or fields.

## Acceptance

- Every field shown on a task can be edited where it is shown. A failed dispatch shows its reason
  on the task within a second of the event.
- fmt, clippy with `-D warnings`, the tests of the crates touched, `cargo test -p pitcrew-protocol
  --features ts`, `npm test`, the UI's checks, both conformance targets, and the guards pass. Every
  CI job passes on the pull request.
