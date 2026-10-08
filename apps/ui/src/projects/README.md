# projects (stream N)

## Task editing and views

Tasks opened from boards and lists use the shared side drawer; direct task URLs retain their
full page. The drawer offers Open full page, Copy link, Mark complete and Edit task. Edit task
updates title, description, priority, dates, labels, workstream and automatic acceptance together;
dependency controls add and remove blockers through the hub's cycle validation. Archived tasks
retain their id, key, plan and history, disappear from work views, and can be restored by the
notification's Undo action or their full page. Archiving closes the drawer so Undo is reachable.

Description Markdown uses React text nodes: emphasis, code, fenced code, headings, bullet lists,
quotes and HTTP(S) links. Raw HTML stays literal text; images never load and unsafe link schemes
stay text. Agent run reads dispatches as well as sessions, shows terminal links only for sessions
with terminals, and reports finished dispatch outcomes and summaries after live invalidation.

My tasks defaults to a list grouped Overdue, Today, Upcoming, No date and Completed (canceled
tasks included in Completed), with a board alternative remembered in local storage per person.
New task accepts explicit project/workstream/status defaults; board columns provide these.
The shell dialog infers project/workstream from the current route. Workstream choices always
belong to the selected project. Description, labels and priority are included on creation.

Dispatchable-agent provisioning/selection uses the merged create-dialogs work. The shell's
search/palette continues to navigate directly to a task page: this brief does not own the palette.

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
| `files.tsx`, `file-viewer.tsx`, `highlight.ts`, `pdf-view.tsx` | The workstream page's Files tab, and its viewer and folder tree, shared with the console's workbench: coloured text, images, PDFs, edit and save. See "Files" below. |
| `my-tasks.tsx`, `projects-list.tsx`, `members.tsx` | My tasks (the board filtered to me), the projects list, and members (people and agents, owners shown). |
| `new-task.tsx` | `NewTaskDialog`: the "+ New" → "Task" item, replacing the shell's placeholder. |
| `inbox.tsx` | `Inbox`: open asks to me by kind, answered in place with stream M's `QuestionCard` (`src/console`); receipts and the task link are the Inbox's own, shown alongside it. |
| `board-draft.tsx`, `board-drafts.ts` | "Draft board" on a workstream (api-v1.md, "Board drafts"): `DraftStart` shows what will be sent (the summary, as text), its size, the number of sessions and redactions, and the agent's estimated usage before anything is; the person picks the agent (their own; the back office first) and the CLI, offered only among those found on the hub's machine, where drafts run (`useDraftEngines`, its `session-options`), and sends; `DraftReview` starts with nothing accepted and lets the person accept or reject each proposed task (or all, or none), and only the accepted ones become tasks, labelled `drafted`. `DraftBoardPanel` is the workstream page's panel; the page also says when a proposal waits. The board types are the generated ones (`packages/protocol-ts/bindings`), re-exported by `board-drafts.ts`. The hooks poll while a draft runs; `board_*` events are new to the data layer, which refetches everything on them. `tests/board-draft.test.tsx` and the browser walk `tests/e2e/board-draft.spec.ts` (in the root `pnpm e2e`, against the mock hub) cover it. |
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
| `writes/` | Outward writes to GitHub and Jira (api-v1.md, "Outward writes"): `api.ts` (`writeClient`, `useTaskWrites`, `useWriteOf`, `useWriteActions`, `fieldRows`), `approval-card.tsx` (`ApprovalCard`, `WriteDiff`: an approval in the Inbox, field by field, with Send and Don't send) and `task-writes.tsx` (`TaskWrites`, the drawer's Upstream section). See "Outward writes" below. |
| `integrations/` | GitHub and Jira (api-v1.md, "Integrations"; read-only upstream): `api.ts` (the wire types, `integrationClient`, `useIntegrations`, `useIntegrationActions`, `useStoreCredential`, and the links' `githubWebRoot`, `scopesOf` and `narrowerScope`), `integrations-page.tsx` (Settings › Integrations at `settings/integrations`: connect, credential, test, sync now, status and problems, linked workstreams, remove) and `workstream-links.tsx` (the workstream page's links upstream and their last sync, and the dialog that links or unlinks them). See "Integrations" below. |

## Integrations

- **The page** lists each connection with its sync's state (`syncState`: syncing, waiting on a rate
  limit, needs a credential, problems, in sync), the last and next sync, the last run's counts, the
  problems (`role="alert"`), and the workstreams it syncs. "Test" shows each check and the
  warnings about a credential that can do more than read. Connecting GitHub offers `gh auth token`
  on the hub's machine or a token entered next; Jira always takes a stored secret.
- **Secrets.** The token field is an uncontrolled password input: on Save its value goes to
  `integrationClient(api).storeCredential` once, through `useStoreCredential` (which keeps only
  whether it is under way and its error, not a TanStack mutation, whose `variables` the mutation
  cache would keep), and the field is cleared, so no React state, query or mutation cache holds it
  (tested). In the desktop app the transport's
  `storeCredential` is the gateway's own command (`gateway_integration_credential`), never
  `gateway_request`; in a browser (development) it is `PUT …/credential`.
- **Links.** A workstream's header shows its links (with the upstream title once synced) and its
  integration's last sync; "Edit links" picks a connected repository or Jira project, optionally
  narrowed to a milestone number or an epic key of that project (anything else is refused before
  it is sent), and `PATCH`es the full list. Links point at the integration's own web host: an
  Enterprise server's origin, not `github.com` (`githubWebRoot`). `workstream_linked`
  refreshes the workstream and the integrations. The list polls every 30 s (2 s while one syncs):
  a sync's status has no event of its own.

## Outward writes

- **In the Inbox**, an `approval` ask the hub raised for a write (`GET /v1/writes/{ask}` answers)
  is an `ApprovalCard`: what it does and to which issue, who or what implied it, a row per field
  sent (upstream's value now, struck through, and exactly what is sent), and the ask's own options
  (Send, Don't send), answered in place through `POST /v1/asks/{id}/answer`. Labels show as the
  change sent (`+ docs, − tests`), never a whole list. Any other ask, an approval an agent raised
  itself included (`404`), stays the console's `QuestionCard`. When the write cannot be read for
  any other reason, the card shows the ask's title and the error, and no buttons: the ask's own
  text is only a short preview of what would be sent.
- **In the task drawer**, "Upstream" lists the task's writes newest first (waiting for approval,
  sent with a link, failed with upstream's message, or not sent and why), each with "What it
  sends" and, when failed, Retry. A person can ask to create an issue from a task that mirrors
  none, or to comment on the one it mirrors; both only propose a write, which waits in the Inbox.
  The section shows when the task mirrors an issue, has writes, or (for a person) when an
  integration is connected.
- Write events invalidate the write queries (`keys.writes`), the asks on a proposal, and the task
  on a result (a created issue becomes its `source`); `write_retry_requested` refreshes the write
  lists and reads as "asked to send a failed write … again" in activity.

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
Its viewer and folder tree are `file-viewer.tsx`, shared with the console's workbench
(`index.ts` exports `FileViewer` and `FileTree` lazily).
Folders load one level at a time; links stay closed, and capped listings are marked.
Up/Down and Home/End move between folder controls; Left/Right close/open folders.
Files show literal UTF-8 text with line numbers, PNG/JPEG images, PDFs, or a byte count.
Images use local Blob object URLs, revoked when the preview changes or unmounts;
SVG files stay binary.

- **Colouring** (`highlight.ts`): a small tokenizer of our own, no dependency. Each language
  is a list of sticky regular expressions; the tokens are kinds and text, rendered as React
  text, so a file never becomes markup. The file's name picks the language (TypeScript and
  JavaScript, JSON, Rust, Go, C and C++, Java and the like, Python, R, Julia, Ruby, Lua, shell,
  TOML and INI, YAML, Markdown, TeX, CSS, HTML and XML, SQL, diffs, Dockerfiles, Makefiles);
  anything else, or a file over 200,000 characters, stays plain. The colours are the tokens'
  (`accent-text`, `ok`, `warn`, `risk`, `ink-2`) on the card background, 4.5:1 or better in
  both themes. Over 2,000 lines only the lines in view are drawn.
- **PDFs** (`pdf-view.tsx`, a lazy chunk): pdf.js's legacy build (`pdfjs-dist`; the modern
  build needs newer JavaScript than our webviews have) draws each page on a canvas as it comes
  into view, with zoom. Only pages are drawn: no annotation, form or link layer, so nothing in
  a file can navigate, run script or reach the network. It fits the desktop CSP as it is: pdf.js
  parses in a worker where one may run, and the same bundled script on the page under the
  desktop's `worker-src 'none'`; WebAssembly is off (`useWasm: false`), so JPEG 2000 and JBIG2
  images, rare outside scans, do not show; fonts load from the file's bytes, never a URL.
- **Edits kept elsewhere.** `FileViewer` keeps its own unsaved edit, or takes one from its caller
  (`draft`, `onDraftChange`) with the revision it was read at, so the workbench can unmount a tab
  without losing it; the save still sends that revision.
Text edits send the revision read. Conflicts keep the draft: Reload discards it after
confirmation, while Overwrite reads the latest revision before trying another write.
Unsaved edits ask before changing files, locations or workstream tabs, and warn on
route navigation and browser unload. A write in progress prevents switches and
route navigation, and warns on unload. Remote locations show
the API's unsupported state. No file contents enter the shared query cache.

`tests/files.test.tsx` covers lazy folders, safe viewers, errors and revisions;
`tests/file-viewer.test.tsx` colouring, long files, PDFs (pdf.js mocked) and kept edits;
`tests/highlight.test.ts` the tokenizer (every character kept, the common constructs coloured).
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

## Start session

A workstream's **Start session** opens the console's shared New session dialog with its
machine and folder prefilled from the workstream's locations. Multiple locations remain
selectable. The console owns launching and opens the resulting session's terminal in the
workbench; this entry does not register another shell create item.

## Creation dialogs

`new-entities.tsx` owns Project, Agent, Team and contextual Workstream entries. Project suggests an
editable key, validates an absolute root for the selected machine's reported platform (WSL uses
Unix paths), and accepts an optional first workstream. Before a machine reports its platform,
only structurally absolute Unix/drive/UNC forms are accepted. The optional workstream is committed
atomically by `POST /v1/projects`; standalone creation uses `POST /v1/workstreams`. Success refreshes
lists and opens the new project/workstream. Agent and Team refresh Members' recipe/team sections
without a reload; newly created personas have owned agent members selectable in teams. Errors
stay in the form. All modal/focus behavior belongs to the shell; pending submissions are disabled.

Run `corepack pnpm --filter @pitcrew/ui exec playwright test -c
src/projects/tests/e2e/create-dialogs.config.ts` for both themes, validation, live lists, palette,
focus and axe. `E2E_HUB_URL` and `E2E_HUB_TOKEN` select a disposable real demo hub for the same tests;
defaults start the mock hub. `tests/create-dialogs.test.tsx` covers the forms and platform roots.

Directory creation uses friendly engine handles, ownership-checked persona edits, and runner-safe model/permission values. Dispatch lists and accepts only persona-linked agents. Every successful scan provisions missing owned agents idempotently. Local setup records OS/architecture and local project roots must be absolute on that platform. Creation dialogs focus Name; tasks default to the current project and filter its workstreams.
