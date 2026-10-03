# projects (stream N)

The Projects layout: where everything stands, what happened lately, and what needs you, with
agents visible where the work is. See `docs/build/streams/N.md` and ADR-0007/0008.

`index.ts` registers the `feature` (routes, nav, commands, "+ New") the shell composes in, and
exports every component **lazily** (render them inside `<Suspense>` if you use them directly), so
none of this folder lands in the initial bundle until a projects route is visited.

| File | What |
|---|---|
| `index.ts` | The feature registration, and the public surface: lazy components (`SessionWork` among them, for the console's session page), `ProjectsNavProvider`, `useInbox` (for the sidebar badge). |
| `layout.tsx` | `ProjectsLayout`: the pathless route every projects route nests under, wiring `ProjectsNavProvider` to the router once (`paths` + `router.navigate`). |
| `data.ts` | Hooks on `useLiveQuery`: inbox (`to=me&state=open`), briefs, activity (`GET /v1/events`), machines, names; mutations: move, assign, create task, subtasks, comment, dispatch, answer ask, edit/pin/keep-current brief, accept a brief proposal. |
| `nav.tsx` | `ProjectsNavProvider`: where "open task / project / workstream / session / receipt / Inbox" go. Without a handler, targets render as plain text. |
| `board.tsx` | `Board` (data, notes; `assignee` for "My tasks") and `BoardView` (columns by status, lanes by workstream, pointer drag, keyboard moves). |
| `moves.ts` | `useOptimisticMoves`: where a moved card shows until the hub and the task list agree. |
| `task-card.tsx` | A card: key, title, assignee (person or agent), priority, due, live status line, **Needs you**. |
| `tasks-list.tsx` | `TasksList`: a flat, sorted list of a workstream's tasks (the workstream page's Tasks tab). |
| `virtual-list.tsx` | Lists over 60 items render through TanStack Virtual, and scroll to an item on request. |
| `task-drawer.tsx` | `TaskDrawer` (Radix dialog) and `TaskDetail`: fields, subtasks (agent-plan lines read-only), agent run, dependencies, comments with @mentions, Work (the task's blocks of work, `WorkBlocks`), history. `TaskDetail`'s `combineHeading` shows "KEY · Title" as one heading, for the task page. |
| `task-page.tsx`, `project-page.tsx`, `workstream-page.tsx` | The routed pages: a task on its own page (not a dialog — see "The task page" below), and a project/workstream with a header and tabs (state, not the URL). |
| `my-tasks.tsx`, `projects-list.tsx`, `members.tsx` | My tasks (the board filtered to me), the projects list, and members (people and agents, owners shown). |
| `new-task.tsx` | `NewTaskDialog`: the "+ New" → "Task" item, replacing the shell's placeholder. |
| `inbox.tsx` | `Inbox`: open asks to me by kind, answered in place with stream M's `QuestionCard` (`src/console`); receipts and the task link are the Inbox's own, shown alongside it. |
| `where-it-stands.tsx` | `WhereItStands`: the brief with receipts; edit and pin for people; the back office's pending proposal (`brief.proposal`), accepted or kept aside — see "Pending proposals" below. |
| `receipts.tsx` | Receipt chips: web links for pull requests, `openReceipt` buttons, a transcript's session (`openSession`) when there is no `openReceipt`, or plain chips otherwise. |
| `activity.tsx` | `ActivityFeed` (Summary / All events, "Load older") and `EventList`. Summary is the recap for the same filters (`recaps.tsx`). In All events, a `project` or `workstream` filter that comes back `400 invalid` (a hub without its activity index) shows "Activity isn't available here yet." instead of an error. |
| `recaps.tsx` | `RecapSummary` (a project's or workstream's day paragraphs, each with its bursts of work, "Load older days"), `WorkBlocks` (a task's or session's blocks of work with their counts, "Load older") and `SessionWork` (`WorkBlocks` under a "Work" heading). See "Recaps" below. |
| `recap-text.tsx` | `SummaryText`: a recap `Summary` as text, every clause a button that opens its evidence. |
| `recap-evidence.ts` | `evidenceFor()`: the sessions, tasks and files a clause's receipts lead to, from the blocks of work it covers. |
| `recap-tz.tsx` | `RecapTzProvider`: the offset day paragraphs use, for tests (the app leaves it to the viewer's own). |
| `agents.tsx` | `AgentsNow`: running sessions and their live status lines. |
| `overview.tsx` | `ProjectOverview`/`WorkstreamOverview` (with their own header, for standalone use and tests) and the header-less `ProjectOverviewBody`/`WorkstreamOverviewBody` the project/workstream pages' Overview tab reuses; `WorkstreamsTable`, `NeedsYouPanel`. |
| `home.tsx` | `Home`; "since you last looked" keeps the last seen revision per workspace in local storage. |
| `format.ts` | Labels, tones, dates, event sentences and a block's counts. |
| `calendar.tsx`, `calendar-dates.ts` | Calendar route: due tasks by month, filters for me/project/workstream, locale week starts, arrow-key day navigation, Enter to show a day's tasks, and a phone agenda. Calendar arithmetic uses UTC solely to keep date-only values stable. |
| `timeline.tsx`, `scheduled-task.tsx`, `schedule.css` | Project Timeline: workstream rows on a shared week/month axis, exact due-date labels, a today line, undated tasks apart, and status symbols and borders. Scrolling stays in the timeline; task buttons open the drawer and regain focus when it closes. |
| `people.tsx`, `ui.tsx` | Avatars; small shared pieces (candidates for `src/design`). |

## The task page

`tasks/$task` renders `TaskDetail` as a plain page (an `h1` reading "KEY · Title", no dialog), not
a modal drawer over the page that opened it: `e2e/shell.spec.ts`'s "Ctrl K opens the palette and
jumps to PAP-4" (already on `main`, outside this stream's paths) asserts no dialog and focus on
`#main` after a direct navigation to a task. `Board`'s own fallback (no `onOpenTask` handler) still
opens `TaskDrawer` as a dialog for standalone use and tests. See the brief's report for the
reasoning.

## Pending proposals

`WhereItStands` reads the back office's pending proposal straight off `brief.proposal` (a
`BriefProposal`: `{ text, next?, receipts, at }`, from `apps/ui/src/data/types.ts`) — it no longer
scans activity for it. `useBrief`'s `brief` only carries a `proposal` when there is one in force to
attach it to (api-v1.md: "A target with a proposal but no brief in force yet is not listed").

Accept and "Keep current" are both a `PUT /v1/briefs/{kind}/{id}` (`useAcceptBrief`/`useSaveBrief`
in `data.ts`, wrapping stream L's `api.acceptBrief`/`api.editBrief`):
- **Accept** sends the proposal's own `text` and `next`. The hub recognizes a `PUT` that matches the
  pending proposal exactly (a missing `next` only matches a missing `next`), copies its `receipts`,
  and keeps `source: 'back_office'`.
- **Keep current** sends the brief's own `text` and `next` unchanged. Even when the text happens to
  match the proposal's (as in the mock's PAP fixture) but the `next` doesn't, the hub treats it as
  the person's own edit: `source: 'person'`, no receipts. Either way the brief accepted is newer
  than the proposal, so nothing is pending afterward — no local "set aside" state needed.

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

Home's "Since you last looked" uses the person's hub cursor for `workspace`, shared
across devices. It marks newer events New and counts them in the loaded activity window;
"Mark all as read" advances to the newest displayed revision. Cursor metadata is excluded.
Project/workstream pages advance their own scope after one second, with the revision
loaded for that visit. Leaving sooner cancels the write; live events during a visit
do not continuously move it. Failed writes show an error. No cursor lives in local storage.
`tests/cursors.test.tsx` covers cross-device refresh and dwell behavior; the cursor browser
tests cover Home, scope visits and axe in both themes. Project browser tests capture no
screenshots by default; optional recap captures require `PITCREW_E2E_SCREENSHOTS=1`.

`useActivity` keeps one live window of `GET /v1/events`. With filters the hub scans a bounded
window per request, so pages can be short or empty without being the end; only `at_start` ends
the feed. One call spends at most 8 requests past what is shown and returns where it stopped;
"Load older" resumes from there.

## Recaps

The Activity tab's **Summary** (project and workstream pages, and the overviews' activity panel)
reads the data layer's `useRecapDays` and `useRecapBlocks` (`src/data/recaps.ts`):

- **Days**, newest first, a heading per date. On a project, one paragraph per workstream within a
  date, under the workstream's name (a link), with the work outside any workstream first; on a
  workstream, one paragraph per date. "Load older days" pages back until `at_start`.
- Under each paragraph, a disclosure ("2 bursts of work") lists the day's blocks, matched by id
  among the scope's blocks, in the order the paragraph tells them: when, the block's line, its
  session and tasks, and its counts. Blocks load lazily and separately from days; a disclosure or a
  clause that needs blocks older than those loaded loads more pages until it has them.
- **Clauses** (`recap-text.tsx`). Text goes through `clauses()` (spans are UTF-8 byte ranges) and is
  rendered as text only, never HTML or markdown. Each clause with receipts is an inline
  `role="button"` (a real `<button>` cannot wrap across lines inside a paragraph) named "*clause*,
  with evidence (*n* receipts)". Hovering it previews its evidence, and the pointer may move into
  the preview to follow a receipt; focusing it previews it too, inert, so Tab goes on to the next
  clause. Enter, Space or a click opens it: a non-modal Radix popover, rendered next to the clause, that
  takes focus and holds the clause's receipts (the existing chips) and the sessions, tasks and
  files they lead to (`recap-evidence.ts`), as links where the layout can go. Tab moves within it;
  Escape closes it and returns to the clause; activating the clause again, or clicking outside,
  closes it too. The joining text is plain.
- **Time zones.** Days use the viewer's own offset (the data layer's default). The mock hub has days
  for `tz=0` only, so the tests set it: `renderWithHub` wraps everything in `<RecapTzProvider tz={0}>`,
  and the Playwright config runs the browser in UTC.

A task's **Work** section (in `TaskDetail`, so on the task page and in the drawer) lists its
blocks of work, newest first, with their lines and counts (files touched with the lines added and
removed, tools run and failed, turns; or the events, when it has none of those) and "Load older".
`SessionWork` is the same for a session, exported lazily from `index.ts` for the console's session
page (stream M's, which this stream does not edit).

## Files

The workstream page's Files tab uses `src/data/files.ts` through the shared transport.
Folders load one level at a time; links stay closed, and capped listings are marked.
Up/Down and Home/End move between folder controls; Left/Right close/open folders.
Files show literal UTF-8 text with line numbers, PNG/JPEG images, or a byte count.
Text edits send the revision read. Conflicts keep the draft: Reload discards it after
confirmation, while Overwrite reads the latest revision before trying another write.
Unsaved edits ask before changing files, locations or workstream tabs, and warn on
browser unload. A write in progress prevents those switches. Remote locations show
the API's unsupported state. No file contents enter the shared query cache.

`tests/files.test.tsx` covers lazy folders, safe viewers, errors and revisions.
`tests/e2e/files.spec.ts` seeds an image through the mock API, then browses, edits,
saves and resolves a conflict, with keyboard and axe checks in both themes.

## Tests

Calendar and Timeline use the same live `useTasks` queries as the board: task date, status and
workstream changes refresh from the stream. New task accepts an optional due date so the Calendar
empty state points to a usable control. Changing dates by dragging is outside this feature.

`tests/calendar-dates.test.ts` covers month/year boundaries, leap day, locale week starts, keyboard
steps and placement on both timeline axes. `tests/calendar-timeline.test.tsx` exercises filters,
the drawer, due-date creation and streamed date/status/workstream changes against the mock hub.
`tests/e2e/calendar-timeline.spec.ts` checks keyboard focus, drawer focus restoration, the phone
agenda, contained scrolling and axe in light and dark themes. It uses its own task date patches,
without changing shared fixtures. To run only these browser checks without screenshots or traces:

```sh
corepack pnpm exec playwright test -c src/projects/tests/e2e/playwright.config.ts calendar-timeline.spec.ts
```

`tests/` runs against the real mock hub (one per test, on a free port) under happy-dom:

```sh
cd apps/ui
corepack pnpm exec vitest run --dir src/projects   # or the package's `test` script, which runs these too
```

`tests/a11y.test.tsx` runs axe on the Board, the task drawer, the Inbox, the overviews, Home, the
Activity Summary (a day's bursts and a clause's evidence open) and a task's Work.
`tests/recaps.test.tsx` covers the Summary of PRJ0001 and of a workstream, clause marking with the
fixture's multi-byte text and with a synthetic summary through a fake `/v1/recaps/days` (emoji,
accents, CJK, and markup that must stay literal), a clause's evidence by keyboard, click and hover,
paging back to the start, a day's blocks loading older pages, and a session's and a task's blocks.

`tests/e2e/` is this stream's Playwright suite, with its own config (the root one's `e2e/` folder
is stream L's): the dev server against its own mock hub on ports 47482 and 5482 (`E2E_HUB_PORT`,
`E2E_UI_PORT`), the browser in UTC.

```sh
cd apps/ui
PLAYWRIGHT_CHANNEL=msedge corepack pnpm exec playwright test -c src/projects/tests/e2e/playwright.config.ts
```
`tests/fake-events.ts` is a `GET /v1/events` feed with the contract's bounded scan, which the mock
hub does not have yet. `tests/routes.test.tsx` wires `feature` into the real shell router
(`createAppRouter`) and checks it serves the shell's placeholders, moves through project →
workstream → task with the paths `src/shell/paths.ts` promises, and that a task page reproduces
from its URL alone. `tests/activity.test.tsx` fakes the real hub's `400 invalid` on a
project/workstream-filtered `GET /v1/events` (the mock accepts them) and checks `ActivityFeed`
shows a note instead of an error, with task/session activity unaffected. `tests/where-it-stands.
test.tsx`'s proposal tests use the mock's demo data (the PAP project brief has a pending proposal)
for Accept and Keep current; a `next` step on the proposal itself is injected at the `fetch` layer
(`withProposedNext`) since that fixture's own proposal has none.
