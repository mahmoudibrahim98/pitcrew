# Brief N · Projects components

- **Stream:** N · UI: Projects layout. **Branch:** `s/N/projects-components`. **Paths:**
  `apps/ui/src/projects/**` only.
- **First read:** [README.md](README.md), then `docs/build/streams/N.md`, ADR-0007, ADR-0008,
  `docs/build/contracts/api-v1.md`, the demo workspace fixture, and the merged UI on `main`:
  **`apps/ui/src/data/README.md` is mandatory** (use `useLiveQuery`), plus `src/design`.

## Goal

The building blocks of the calm "where does everything stand" layout, as tested components
against the mock hub. The shell's feature registration arrives in brief L-shell. Until then,
**build components, not routes**; the next N brief wires them in.

## What to build (all under `src/projects`)

1. **Data hooks** (`src/projects/data.ts`), on `useLiveQuery`:
   - projects, workstreams, tasks (with filters), asks (Inbox: `to=me&state=open`), briefs,
     members;
   - activity (`GET /v1/events` paging);
   - mutations: move, assign, create task, comment, answer ask, edit and pin brief.
2. **`WhereItStands`:** a brief with its receipts (each receipt a link or chip: transcript
   offset, commit, job, file, event); edit and pin for people; "proposed" state when the back
   office proposes.
3. **`Board`:**
   - task columns by status, or grouped by workstream;
   - drag and drop calls `move`; a 409 snaps back with the server's message;
   - cards show key, title, assignee avatar (person or agent), priority, due, a live agent
     status line, and a **Needs you** badge when an open ask exists for the task;
   - virtualised columns.
4. **`TaskDrawer`:**
   - fields; subtasks, with agent-plan lines marked and read-only for people;
   - dependencies, comments with @mentions, history from activity;
   - an **Agent run** section (the session's live state; placeholders for opening chat or the
     terminal; dispatch to an agent).
5. **`Inbox`:** asks grouped by kind (question, decision, review, approval, mention), answerable
   in place, with receipts. Reuse the console's `QuestionCard` if stream M has merged it;
   otherwise build a local one and note it.
6. **`ProjectOverview` and `WorkstreamOverview`** composites: Where it stands, the workstreams
   table (status, health, next), recent activity (Summary / All events toggle; Summary can be a
   placeholder for now), and the agents working on it.
7. **`Home`:** where things stand across projects, agents now, what needs you, and what changed
   since you last looked (last-seen revision in local storage for now).
8. Export these from `src/projects/index.ts` as named components, keeping L's feature stub if
   present.

## Acceptance

- Vitest and Testing Library, against the real mock hub on port 0:
  - the board shows the demo tasks; a person's move succeeds;
  - a move `can_move` rejects shows the 409 message and snaps back;
  - Needs you badges match open asks and clear when an ask is answered;
  - the task drawer shows PAP-1's agent-plan subtasks as read-only;
  - the Inbox answers an ask in place;
  - Where it stands renders the fixture briefs with receipts.
- Live: a `task_moved` from another client moves the card without a reload.
- axe reports no violations for Board, TaskDrawer and Inbox; everything is keyboard operable
  (drag has a keyboard alternative).
- `typecheck`, `lint` and `build` pass; projects code is lazy-loaded.

## Out of scope

Routes and navigation (next brief), Calendar, Timeline, Members, and the Summary recaps' content
(stream F).
