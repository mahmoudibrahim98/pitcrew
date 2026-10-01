# projects (stream N)

The Projects layout: where everything stands, what happened lately, and what needs you, with
agents visible where the work is. See `docs/build/streams/N.md` and ADR-0007/0008.

Components only for now; the routes come with the next N brief, on the shell's feature
registration. `index.ts` exports every component **lazily** (render them inside `<Suspense>`), so
none of this folder lands in the initial bundle.

| File | What |
|---|---|
| `index.ts` | Public surface: lazy components, `ProjectsNavProvider`, `useInbox` (for the sidebar badge). |
| `data.ts` | Hooks on `useLiveQuery`: inbox (`to=me&state=open`), briefs, activity (`GET /v1/events`), machines, names; mutations: move, assign, create task, subtasks, comment, dispatch, answer ask, edit/pin brief. |
| `nav.tsx` | `ProjectsNavProvider`: where "open task / project / workstream / session / receipt / Inbox" go. Without a handler, targets render as plain text. |
| `board.tsx` | `Board` (data, notes) and `BoardView` (columns by status, lanes by workstream, pointer drag, keyboard moves). |
| `moves.ts` | `useOptimisticMoves`: where a moved card shows until the hub and the task list agree. |
| `task-card.tsx` | A card: key, title, assignee (person or agent), priority, due, live status line, **Needs you**. |
| `virtual-list.tsx` | Lists over 60 items render through TanStack Virtual, and scroll to an item on request. |
| `task-drawer.tsx` | `TaskDrawer` (Radix dialog) and `TaskDetail`: fields, subtasks (agent-plan lines read-only), agent run, dependencies, comments with @mentions, history. |
| `inbox.tsx` | `Inbox`: open asks to me by kind, answered in place. |
| `question-card.tsx` | A **local** `QuestionCard` (stream M's is not merged yet). |
| `where-it-stands.tsx` | `WhereItStands`: the brief with receipts; edit and pin for people; the back office's pending proposal. |
| `receipts.tsx` | Receipt chips: web links for pull requests, `openReceipt` buttons or plain chips otherwise. |
| `activity.tsx` | `ActivityFeed` (Summary placeholder / All events, "Load older") and `EventList`. |
| `agents.tsx` | `AgentsNow`: running sessions and their live status lines. |
| `overview.tsx` | `ProjectOverview`, `WorkstreamOverview`, `WorkstreamsTable`, `NeedsYouPanel`. |
| `home.tsx` | `Home`; "since you last looked" keeps the last seen revision per workspace in local storage. |
| `format.ts` | Labels, tones, dates and event sentences. |
| `people.tsx`, `ui.tsx` | Avatars; small shared pieces (candidates for `src/design`). |

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
hub does not have yet.
