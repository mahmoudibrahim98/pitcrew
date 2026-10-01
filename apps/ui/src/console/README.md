# console (stream M)

The Agent console. See `docs/build/streams/M.md`. `index.ts` registers it with the shell (`feature`)
and exports its components. The app imports `index.ts` at start, so it stays small: the page and
every component are lazy chunks (render the components inside a `<Suspense>`).

| File | What |
|---|---|
| `index.ts` | `feature`: the routes `console` and `console/$session` (one lazy page for both) and the palette commands. The lazy component exports. |
| `console-page.tsx` | `ConsolePage`: filters, list and session side by side, or one at a time when narrow. See [The page](#the-page). |
| `search.ts` | The facets in the URL's search (`?machine=A,B&state=waiting`). No React. |
| `panes.ts` | The pane widths and whether the filters show, persisted in this browser as `pitcrew.console`. |
| `intent.ts` | Hands a palette command's request (show the list, open a filter, filter by state) to the page, at once or as it mounts. No React. |
| `session-list.tsx` | `SessionList` (live from the hub) and `SessionListView` (from data it is given): virtualised, grouped by project → workstream plus *Unsorted*, a "Starting…" row, listbox keyboard navigation. `onSelect(session, via)` says whether a click or the keyboard chose; `onActiveChange` reports the arrows' moves. It follows (and scrolls to) a selection made elsewhere. |
| `session-filters.tsx`, `facets.ts` | `SessionFilters`: machine, engine, state, project and workstream facets with counts. Controlled; pass the value to `SessionList`. |
| `chat-view.tsx`, `chat-rows.tsx` | `ChatView`: one session's transcript, newest page first, virtualised and anchored to its end. Older pages load on scroll-up (or when the view is not full) without moving the rows in view. |
| `question-card.tsx` | `QuestionCard`: an ask (answered through `POST /v1/asks/{id}/answer`), or a live transcript question with no ask (an option is picked with arrow keys and Enter; free text is sent as a prompt). |
| `composer.tsx` | `Composer`: Enter sends, Shift+Enter adds a line; Esc, Ctrl+C, Stop; disabled with the reason when the session has ended or cannot be reached (also after a 503). |
| `session-header.tsx` | `SessionHeader`: title, state, engine, agent, machine, branch, folder, task and workstream links (real links when given `taskHref` and `workstreamHref`), actions (End; Hand off, Fork and Review are disabled until given handlers). |
| `data.ts` | Hooks on `useLiveQuery` and `src/data`'s API client and types (see `src/data/README.md`): sessions with facets, the transcript window, pending prompts, and the send, keys, interrupt, end and answer mutations. Machines come from `src/data`'s `useMachines`. |
| `transcript.ts` | `TranscriptWindow` (merges pages by record offset; reports gaps) and `buildRows` (tool calls paired by `call_id`, questions folded with the call that asked them). No React. |
| `render/` | Markdown and diff parsing in a web worker (`worker.ts`, `client.ts`); the parser loads on the main thread only where no worker runs. `markdown.tsx` renders the parsed tree as elements: no HTML string anywhere, links only for http, https and mailto (`links.tsx`), opening outside the app; images are never loaded. |

## The page

- **Where things are kept.** The chosen session is in the path (`/w/$ws/console/$session`), the
  filters in the search, so a link or a reload reproduces the view. Choosing a session with a
  click or Enter adds a history entry; the arrows' choices and filter changes replace it. Pane
  widths are per browser (`panes.ts`).
- **Layout.** Wider than 720 px (the console, not the window), the filters and the list are
  `ResizablePanel`s beside the session; "Filters" in the list's header shows or hides the filters.
  Narrower, one pane shows at a time: the list, the filters or the session, each with a way back
  to the list.
- **Keys.** F6 and Shift+F6 move between the panes: the filters, the list, the transcript and the
  composer (those on screen). In the list, the arrows, Page keys, Home and End choose the session
  beside it once they rest on one (150 ms); Enter or Space opens it and goes to the composer.
  The console claims none of the shell's keys (`ownsShellKeys` is not used): in the composer,
  Ctrl B, Ctrl J and Ctrl . stay with the text field, and Ctrl K opens the palette as everywhere.
- **Palette.** "Go to the Agent console", "Jump to a session…", "Filter sessions by machine…" and
  "… by state…", "Show sessions waiting for input", "Show working sessions" and "Clear the session
  filters", in both layouts. A command goes to the console if needed and hands its request over
  through `intent.ts`.
- **Links to the work.** The header links to the task (`paths.task`) and the workstream
  (`paths.workstream`) by path, so they open whatever serves those paths (the Projects layout).
- **No "+ New" item.** Starting a session needs a flow that does not exist yet, and the shell's
  `CreateEntry` cannot be shown disabled with a reason.

## Transcripts

The newest page is `keys.sessions.transcript(id)`, the only one the stream refetches
(`session_state_changed`, `turn_ended`, `tool_ran` and `file_edited` touch it); older pages are
`keys.sessions.transcriptPage(id, before)` and never refetch. `TranscriptWindow` keeps every
record it has seen and which byte ranges are complete, so a newer tail never drops what the user
is reading, and a gap (more than a page arrived between two fetches) is fetched on its own.

## Links

Links open outside the app. The desktop shell passes its opener with
`<OpenExternalProvider open={…}>`; without it, links are plain `target="_blank"
rel="noopener noreferrer"` anchors.

## Colours

Small text uses `ink` or `ink-2`, never `muted` (an icon colour, about 3.5:1 on light surfaces).
The progress colour is too light for text in the light theme, so a working state shows as a
coloured dot beside `ink-2` text.

## Tests

- **Vitest** (`src/console/tests`), run by the package's `test` script (or on their own with
  `corepack pnpm --filter @pitcrew/ui exec vitest run src/console`). Component tests use happy-dom
  and the real mock hub as a child process on a free port; `stubLayout()` gives virtual lists a
  size. `console-page.test.tsx` mounts the shell's router with the console feature, in Strict Mode
  as `src/main.tsx` does.
- **Playwright** (`src/console/tests/e2e`): the acceptance run in the real app against the mock
  hub, axe included (both themes, both layouts), on ports 47450 (hub) and 47451 (UI), which
  `E2E_HUB_PORT` and `E2E_UI_PORT` move. Traces are kept only for failures, in
  `apps/ui/test-results/console`.

  ```sh
  cd apps/ui
  PLAYWRIGHT_CHANNEL=msedge corepack pnpm exec playwright test -c src/console/tests/e2e/playwright.config.ts
  ```
