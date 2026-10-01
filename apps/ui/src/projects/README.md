# projects (stream N)

The Projects layout: where everything stands, what happened lately, and what needs you, with
agents visible where the work is. See `docs/build/streams/N.md` and ADR-0007/0008.

`index.ts` registers the `feature` (routes, nav, commands, "+ New") the shell composes in, and
exports every component **lazily** (render them inside `<Suspense>` if you use them directly), so
none of this folder lands in the initial bundle until a projects route is visited.

| File | What |
|---|---|
| `index.ts` | The feature registration, and the public surface: lazy components, `ProjectsNavProvider`, `useInbox` (for the sidebar badge). |
| `layout.tsx` | `ProjectsLayout`: the pathless route every projects route nests under, wiring `ProjectsNavProvider` to the router once (`paths` + `router.navigate`). |
| `data.ts` | Hooks on `useLiveQuery`: inbox (`to=me&state=open`), briefs, activity (`GET /v1/events`), machines, names; mutations: move, assign, create task, subtasks, comment, dispatch, answer ask, edit/pin brief. |
| `nav.tsx` | `ProjectsNavProvider`: where "open task / project / workstream / session / receipt / Inbox" go. Without a handler, targets render as plain text. |
| `board.tsx` | `Board` (data, notes; `assignee` for "My tasks") and `BoardView` (columns by status, lanes by workstream, pointer drag, keyboard moves). |
| `moves.ts` | `useOptimisticMoves`: where a moved card shows until the hub and the task list agree. |
| `task-card.tsx` | A card: key, title, assignee (person or agent), priority, due, live status line, **Needs you**. |
| `tasks-list.tsx` | `TasksList`: a flat, sorted list of a workstream's tasks (the workstream page's Tasks tab). |
| `virtual-list.tsx` | Lists over 60 items render through TanStack Virtual, and scroll to an item on request. |
| `task-drawer.tsx` | `TaskDrawer` (Radix dialog) and `TaskDetail`: fields, subtasks (agent-plan lines read-only), agent run, dependencies, comments with @mentions, history. `TaskDetail`'s `combineHeading` shows "KEY · Title" as one heading, for the task page. |
| `task-page.tsx`, `project-page.tsx`, `workstream-page.tsx` | The routed pages: a task on its own page (not a dialog — see "The task page" below), and a project/workstream with a header and tabs (state, not the URL). |
| `my-tasks.tsx`, `projects-list.tsx`, `members.tsx` | My tasks (the board filtered to me), the projects list, and members (people and agents, owners shown). |
| `new-task.tsx` | `NewTaskDialog`: the "+ New" → "Task" item, replacing the shell's placeholder. |
| `inbox.tsx` | `Inbox`: open asks to me by kind, answered in place with stream M's `QuestionCard` (`src/console`); receipts and the task link are the Inbox's own, shown alongside it. |
| `where-it-stands.tsx` | `WhereItStands`: the brief with receipts; edit and pin for people; the back office's pending proposal (still read by scanning events — see "Pending proposals" below). |
| `receipts.tsx` | Receipt chips: web links for pull requests, `openReceipt` buttons or plain chips otherwise. |
| `activity.tsx` | `ActivityFeed` (Summary placeholder / All events, "Load older") and `EventList`. A `project` or `workstream` filter that comes back `400 invalid` (the real hub, until its index lands) shows "Activity isn't available here yet." instead of an error. |
| `agents.tsx` | `AgentsNow`: running sessions and their live status lines. |
| `overview.tsx` | `ProjectOverview`/`WorkstreamOverview` (with their own header, for standalone use and tests) and the header-less `ProjectOverviewBody`/`WorkstreamOverviewBody` the project/workstream pages' Overview tab reuses; `WorkstreamsTable`, `NeedsYouPanel`. |
| `home.tsx` | `Home`; "since you last looked" keeps the last seen revision per workspace in local storage. |
| `format.ts` | Labels, tones, dates and event sentences. |
| `people.tsx`, `ui.tsx` | Avatars; small shared pieces (candidates for `src/design`). |

## The task page

`tasks/$task` renders `TaskDetail` as a plain page (an `h1` reading "KEY · Title", no dialog), not
a modal drawer over the page that opened it: `e2e/shell.spec.ts`'s "Ctrl K opens the palette and
jumps to PAP-4" (already on `main`, outside this stream's paths) asserts no dialog and focus on
`#main` after a direct navigation to a task. `Board`'s own fallback (no `onOpenTask` handler) still
opens `TaskDrawer` as a dialog for standalone use and tests. See the brief's report for the
reasoning.

## Pending proposals

`WhereItStands` still finds a back-office proposal by scanning activity (`pendingProposal` in
`where-it-stands.tsx`): `integrator/work-edits`, which adds `Brief.proposal` to `GET /v1/briefs`,
had not merged into `main` as of this brief. Once it has, switch to reading it directly and make
"Keep current" a `PUT` of the current text (see the brief).

## How moves work

A drop or a keyboard move shows the card in its new column at once and calls
`POST /v1/tasks/{id}/move`. The cache is not touched: the `task_moved` event refreshes the lists.
- If the hub refuses (409 `can_move`, 403), the card goes back and the board shows the hub's
  message, one note per task.
- If it accepts, the card stays put until the task list shows the task anywhere but where it
  started (a slow refresh from before the move does not send it back), or for at most 60 s, the
  stream's reconnect window; then the list wins.

Dragging uses pointer events, not HTML5 drag and drop, which Tauri's webview on Windows intercepts
for file drops.

Keyboard: each card's move button picks the card up (Enter or Space), the left and right arrows
choose a column, Enter or Space drops it, Escape cancels; a live region announces each step. The
moved card takes focus where it shows next (a virtualised column scrolls to it), and again if the
hub sends it back.

## Activity

`useActivity` keeps one live window of `GET /v1/events`. With filters the hub scans a bounded
window per request, so pages can be short or empty without being the end; only `at_start` ends
the feed. One call spends at most 8 requests past what is shown and returns where it stopped;
"Load older" resumes from there.

## Tests

`tests/` runs against the real mock hub (one per test, on a free port) under happy-dom:

```sh
cd apps/ui
corepack pnpm exec vitest run --dir src/projects   # or the package's `test` script, which runs these too
```

`tests/a11y.test.tsx` runs axe on the Board, the task drawer, the Inbox, the overviews and Home.
`tests/fake-events.ts` is a `GET /v1/events` feed with the contract's bounded scan, which the mock
hub does not have yet. `tests/routes.test.tsx` wires `feature` into the real shell router
(`createAppRouter`) and checks it serves the shell's placeholders, moves through project →
workstream → task with the paths `src/shell/paths.ts` promises, and that a task page reproduces
from its URL alone. `tests/activity.test.tsx` fakes the real hub's `400 invalid` on a
project/workstream-filtered `GET /v1/events` (the mock accepts them) and checks `ActivityFeed`
shows a note instead of an error, with task/session activity unaffected.
