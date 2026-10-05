# console (stream M)

The Agent console. See `docs/build/streams/M.md`. `index.ts` registers it with the shell (`feature`)
and exports its components. The app imports `index.ts` at start, so it stays small: the page and
every component are lazy chunks (render the components inside a `<Suspense>`).

| File | What |
|---|---|
| `index.ts` | `feature`: the routes `console` and `console/$session` (one lazy page for both) and the palette commands. The lazy component exports. |
| `console-page.tsx` | `ConsolePage`: filters, list and the workbench side by side, or one at a time when narrow. See [The page](#the-page). |
| `workbench/` | The workbench: sessions, terminals and files in tabs and splits, the details sidebar. See [The workbench](#the-workbench). |
| `search.ts` | The facets in the URL's search (`?machine=A,B&state=waiting`), and the session pane's view (`?view=terminal`, `?view=work`). No React. |
| `view-switch.tsx` | `ViewSwitch`: the session pane's Chat \| Terminal \| Work switch; without a terminal, Terminal stays focusable but does nothing, and the reason shows beside it. Work is always available. |
| `terminal/` | The terminal, a lazy chunk with xterm.js in it. See [The terminal](#the-terminal). |
| `work-boundary.tsx` | `WorkBoundary`: the error boundary around `SessionWork`'s lazy chunk (`src/projects`), with "Try again" — the same shape as `TerminalBoundary`. |
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
  widths are per browser (`panes.ts`); the workbench's layout is per workspace
  (`workbench/store.ts`).
- **Layout.** Wider than 720 px (the console, not the window), the filters and the list are
  `ResizablePanel`s beside the workbench; "Filters" in the list's header shows or hides the filters.
  Narrower, one pane shows at a time: the list, the filters or the session in the URL (not the
  workbench: its panes need the width), each with a way back to the list.
- **Chat, terminal or work.** Under the header, a Chat | Terminal | Work switch. The terminal and
  work views are in the URL's search (`?view=terminal`, `?view=work`, replacing the history entry),
  so a link or a reload shows them again; choosing another session keeps the choice. A session
  without a `terminal` shows the chat, and the switch's Terminal option is disabled with the reason
  ("This session has no terminal."); Work is always available, even for a session with no bursts of
  work yet. In a narrow console the terminal and work views take the session pane: the header keeps
  only its title row. Work mounts `SessionWork` (`src/projects`, lazy, under a `<Suspense>` and
  `WorkBoundary`) as a third view rather than a collapsible section under the header, because it is
  an alternative way to read the session — like the chat and the terminal, not a supplement to one
  of them — and reuses the pane-switching machinery (the URL, F6, the narrow layout's rules) the
  other two already have, instead of adding a second, different kind of toggle. The console gives
  it no `ProjectsNavProvider` (that is `ProjectsLayout`'s, `src/projects`), so a block's task shows
  as plain text, not a link, there; the session itself never shows (the console already is that
  session's page).
- **Keys.** F6 and Shift+F6 move between the panes on screen, in the order they are on screen:
  the filters, the list, then each workbench pane's transcript and composer (or its terminal, work
  view or file in their place), then the details. In the list, the
  arrows, Page keys, Home and End choose the session beside it once they rest on one (150 ms); Enter
  or Space opens it and goes to the composer. Only a terminal in control mode claims the shell's
  keys (`ownsShellKeys`), and it keeps F6 too. In the composer, Ctrl B, Ctrl J and Ctrl . stay with
  the text field, and Ctrl K opens the palette as everywhere.
- **Palette.** "Go to the Agent console", "Jump to a session…", "Filter sessions by machine…" and
  "… by state…", "Show sessions waiting for input", "Show working sessions" and "Clear the session
  filters", in both layouts; the workbench's commands (below) in the console's. A command goes to
  the console if needed and hands its request over through `intent.ts`.
- **Links to the work.** The header links to the task (`paths.task`) and the workstream
  (`paths.workstream`) by path, so they open whatever serves those paths (the Projects layout).
- **Start session.** The console feature registers the lazy Session create entry, and its list
  and each workstream offer Start session. All three use `new-session.tsx`: machine-scoped
  installed engines and allowed modes from session-options, platform-aware absolute paths,
  optional prompt/title, inline errors retaining input, and navigation to the new terminal tab.
  Workstream locations prefill the machine/folder; the runner still checks safety and existence.
  `tests/new-session.test.tsx` covers all entries and errors against the mock hub;
  `tests/e2e/start-sessions.spec.ts` also supports a disposable real hub through the root browser
  config's E2E_HUB_URL/TOKEN and E2E_SESSION_CWD. A mechanical root e2e import includes it in CI.

## The workbench

The session area is an IDE's editor area (`workbench/`): **panes** of **tabs**, side by side or
stacked, each tab a session (its chat, terminal or work, with its own Chat | Terminal | Work
switch) or a file of a workstream's folder.

| File | What |
|---|---|
| `workbench/layout.ts` | The model, no React: panes in nested splits (`row` or `column`, each child's share in `sizes`), tabs, the active pane; open, reveal, close, split, move, drop, resize; `parseLayout`, a strict check of a stored layout. Unit tests: `tests/workbench-layout.test.ts`. |
| `workbench/store.ts` | The layout per workspace in local storage (`pitcrew.workbench.<ws>`, version 1). Missing, unreadable or malformed (any part of it) gives the empty layout; storage that throws is ignored. |
| `workbench/workbench.tsx` | `useWorkbench` (the actions) and `Workbench`: panes, tab strips, separators, drag and drop, the details sidebar. |
| `workbench/session-tab.tsx` | `SessionPane`: a session's header, view switch and view (also the narrow console's session pane). |
| `workbench/file-tab.tsx` | A file in a pane: `FileViewer` from `src/projects` (the Files tab's), its unsaved edit in `drafts.ts`. |
| `workbench/drafts.ts` | Unsaved edits by tab, in memory only, with the revision each was read at. |
| `workbench/details.tsx` | The details sidebar. |
| `workbench/keys.ts` | The keys and each one's palette command. |

- **Opening.** A session chosen in the list (click, or the arrows resting on it) is shown where it
  is open already, in the view it has there; otherwise it opens in the active pane as its
  **preview** tab (in italics), which the next one replaces, so following the list does not pile
  up tabs. Enter in the list, a double-click on the tab, its menu's "Keep open", switching its view,
  moving it or splitting it keeps it. A row's menu has "Open in a new tab" and "Open to the side".
  A link, the history or the palette's "Jump to a session" shows the session in the view the URL
  names, switching an open tab of it if need be.
- **The URL** follows the tab on screen in the active pane: a session and its view, or the bare
  console for a file (or nothing). Switching tabs replaces the history entry.
- **Splitting.** "Split right" and "Split down" on a pane (or its tab's menu) open a copy of its tab
  in a new pane beside it, as an editor does; panes of the same direction share one split. At most
  16 panes. Closing a pane's last tab closes the pane; the only pane stays, empty.
- **Dragging.** A tab dragged onto another pane's tab strip goes there at that place; onto a pane's
  edge (its outer quarter) into a new pane on that side; onto its middle into it. Where the target
  pane already shows the same thing, that tab comes on screen instead. Only the workbench's own tabs
  are taken; anything else dragged in is left to the browser.
- **Resizing.** The boundary between panes is a separator (role `separator`, its position as
  `aria-valuenow`): drag it, or focus it and use the arrows (5% a step), Home and End. No pane
  gets under 10% of its split.
- **Persisted.** Panes, tabs, sizes, the active pane and the details' state and width are saved per
  workspace on every change and read back on load, so a reload shows the same workbench. What is
  shown is checked again as it loads: a session that is gone says so in its tab, a file says "Gone".
  File contents are never stored.
- **Unsaved edits.** A file's tab keeps its edit while another tab is on screen (only the tab on
  screen in each pane is rendered). Closing it asks first; so does closing its pane or the others;
  leaving the page warns. Saving sends the revision the edit started from, so a change made
  meanwhile is a conflict (Reload, Overwrite), as in the Files tab.
- **Details** (each pane's "Details" button, or the palette): for a session, its task, workstream,
  machine, state, model and account, with "Link to a task…", "Hand off" (shown, and saying it is not
  available yet: there is no route for it), "Terminal beside" and "Chat beside", and its
  workstream's folders, to open files from. The hub reports no model or account on a session: the
  model shown is the agent's persona's, when it names one, otherwise "Not reported"; the account is
  always "Not reported". For a file: its workstream, folder and path, and the folders.
- **Accessibility.** Each pane is a region ("Pane 2"); its tab strip a `tablist` ("Tabs in pane 2")
  of `tab`s controlling a `tabpanel`, one tab stop per pane (arrows, Home, End; Shift with an arrow
  moves the tab; Delete closes it; Enter goes into it; the Menu key or a right-click opens its
  actions; a description on each tab says so). A tab's close button is for the mouse and hidden
  from assistive technology, as Delete does the same. A pane's chat and "Linked work" landmarks
  carry the pane's number from the second pane on, so no two landmarks share a name.

### Keys

Alt (Option on macOS) with PageDown or PageUp shows the next or previous tab of the active pane;
with Shift, moves the tab on screen to the next or previous pane (making one when there is only
one). Alt W closes the tab on screen; Alt \ and Alt Shift \ split the active pane right and down.
Each is also a palette command (group "Workbench"), with "Next pane", "Previous pane" (F6, Shift
F6) and "Show or hide the session details".

- **Why these.** A browser keeps Ctrl W, Ctrl Tab, Ctrl PageDown and Ctrl 1 to 9 for itself (the
  page never sees them), and the shell owns Ctrl K, J, B and .; Alt with PageDown, PageUp, W and \
  means nothing to Chromium, Firefox, WebKit or the desktop's webviews.
- **Where they act.** Alt PageDown and PageUp act anywhere in the workbench; Alt W and Alt \ not
  in a text field (on macOS they type characters there). A key-owning surface (a terminal in
  control mode) keeps every key.

## Transcripts

The newest page is `keys.sessions.transcript(id)`, the only one the stream refetches
(`session_state_changed`, `turn_ended`, `tool_ran` and `file_edited` touch it); older pages are
`keys.sessions.transcriptPage(id, before)` and never refetch. `TranscriptWindow` keeps every
record it has seen and which byte ranges are complete, so a newer tail never drops what the user
is reading, and a gap (more than a page arrived between two fetches) is fetched on its own.

## The terminal

A session with a `terminal` shows it live: xterm.js on `GET /v1/sessions/{id}/terminal`
(`docs/build/contracts/api-v1.md`, "Terminals"). `TerminalView` is a lazy chunk, and xterm is in
that chunk alone: nothing of it loads until a terminal is on screen (`index.ts` also exports it,
lazily, for the workbench).

| File | What |
|---|---|
| `terminal/socket.ts` | `TerminalSocket`, no React: the connection, reconnects by offset, keystrokes and resizes (below), on sockets from the data layer's `useOpenSocket()`. |
| `terminal/diagnose.ts` | Why the hub refused a terminal, asked over HTTP (below). |
| `terminal/controller.ts` | `TerminalController`, no React: xterm, its addons, fit, theme and font, input by mode, flow control, and disposal. |
| `terminal/terminal-view.tsx` | `TerminalView`: the mode, Take control and Release, the status and Try again, the screen reader setting, the truncated marker, and the frame xterm draws in. |
| `terminal-boundary.tsx` | `TerminalBoundary`: the error boundary around the terminal's lazy chunk, with Try again. |
| `terminal/options.ts` | xterm's options for hostile output, and the link handler. |
| `terminal/keys.ts` | The release chord and view mode's keys. |
| `terminal/theme.ts`, `terminal/contrast.ts` | Colours and font from the design tokens; dim text that keeps 4.5:1. |
| `terminal/prefs.ts` | The screen reader mode, per browser (`pitcrew.terminal`). |

### The connection

- **Sockets by path, through the data layer.** `TerminalSocket` asks an opener for an API path
  (the data layer's `terminalPath(session, { cols, rows, from })`), never a URL, and the opener is
  `useOpenSocket()` (`src/data/README.md`, "Sockets for features"): a browser WebSocket in
  development, with the token only as the `pitcrew.bearer.` subprotocol and never in the URL; the
  desktop gateway in the app, which adds the token itself, so the console reads no token at all.
  Tests pass a fake through `TerminalView`'s `socket` prop. An opener's sockets deliver text as
  strings and binary frames as `ArrayBuffer`s (typed arrays are read too; a `Blob` is not), and
  report every close, the one asked for included, so the terminal detaches its handlers before it
  closes a socket.
- **No byte lost or repeated.** It counts the output bytes received and reconnects with
  `from=<count>`. `{"type":"truncated","from":N}` moves the count to N (the hub no longer has the
  bytes between), and the view shows "Earlier output is no longer available." as a line at the top
  of the terminal. The marker is page text, not bytes written into the terminal: the program's
  screen is left as it drew it, and a clear-screen cannot erase the marker. Only a CAN (0x18) goes
  to xterm there, uncounted, so an escape sequence the gap cut cannot swallow the output after it.
- **The end.** `{"type":"exit"}` or a 1000 close: "The program ended." It never reconnects.
- **Close codes.** 1007, 1009 and 1011 (and 1002, 1003, 1008) stop with the reason, the close's
  own reason after it. 1011 is the hub's runtime failing, or the desktop gateway failing to send a
  frame ("The terminal failed (send failed).") 1001, 1013, 1006 and anything else reconnect.
- **A refused upgrade.** A browser shows every refused upgrade as a 1006 close before `open`; the
  HTTP status is hidden. So after one, `terminalDiagnosis` asks the hub as it decides: the session
  (404: "This session no longer exists."), its machine (503: "gpu-box cannot be reached right now,
  so its terminal cannot be shown."), its terminal (404: "This session has no terminal."). Any of
  those stops it, with the reason.
  - The hub's own 503 on `GET /v1/sessions/{id}` is the machine's reason only while that machine
    is not live; otherwise (a hub restarting) it reconnects.
  - If the hub cannot be asked, or does not answer within 10 s (the requests are aborted), nothing
    is found, and it reconnects.
  - If the hub answers and nothing explains the refusal, it reconnects, but after 5 such refusals
    in a row it stops: "The hub refused the terminal."
  - In the desktop app the gateway already says why (the close carries its `GatewayError`), so
    the terminal stops with that reason and asks the hub nothing.
- **An attempt that hangs.** One that has not opened after 10 s (a stuck hub, a half-open tunnel)
  is closed and treated as refused, so "Connecting…" never lasts.
- **Back-off.** Attempt n waits `min(15 s, 500 ms × 2ⁿ)` times 0.5–1 (jitter), and starts over
  once a connection has stayed up for 5 s. While the page is hidden or the browser offline it does
  not reconnect at all; it reconnects at once when the page is visible and online again.
- **Trying again.** A stopped terminal has a "Try again" button, which reconnects from the bytes
  received with the back-off starting over. It also tries again by itself when the session's
  machine comes back (its liveness returns to `live`) or, in the desktop app, when the gateway's
  workspace is `ready` again. Nothing else restarts it, so a stop is never a loop.
- **Keystrokes** are binary frames of at most 64 KiB (UTF-8 from `TextEncoder`; X10 mouse reports
  byte for byte). While connected, up to 1 MiB at once (a paste) is sent, in 64 KiB frames, unless
  as much is still waiting to go out on a slow connection. While disconnected they wait, 64 KiB at
  most in all, and go out in order on reconnecting. Past those limits they are refused and the
  view says so, until keystrokes go through again. Nothing is buffered without limit, and no
  message can reach the hub's 1 MiB limit.
- **Resize.** xterm is fitted to its element (`@xterm/addon-fit` and a `ResizeObserver`); the size
  goes to the hub 100 ms after it settles, clamped to 1..=1000, and only when it changed. The first
  connection already carries the fitted size.
- **Flow control.** Output given to xterm and not yet parsed is counted. Past 4 MiB the socket
  holds (closes) and reconnects from the bytes received once xterm is down to 512 KiB, so a flood
  neither grows xterm's write queue without bound nor loses output. It shows "Catching up with the
  output…", never "The connection dropped". (What xterm keeps once parsed: see "Bounded memory"
  below.)

### View and control

- **View mode** is the default. xterm's input is off (`disableStdin`) and it acts on no key, and
  focus stays on the terminal's frame (never in xterm's text field), so every shell shortcut works
  as anywhere else. Enter takes control; the arrows, Page Up, Page Down, Home and End scroll; Ctrl+C
  (Cmd+C on macOS) copies a selection.
- **Control mode** is taken with "Take control" or Enter on the focused terminal. The frame then
  spreads `ownsShellKeys`, and the shell's keys are no longer the shell's: Esc, Tab, Ctrl K, J and
  B go to the program. Ctrl . is taken from the shell too, but a terminal has no code for it, so
  nothing is sent. In a browser tab, Ctrl W, T and N stay the browser's (it closes or opens a tab)
  and never reach the page, let alone the program. Control is left with "Release" or
  **Ctrl+Shift+X** (Ctrl, not Cmd, on every platform, as terminal keys are), which never reaches
  the program.
  - **Why Ctrl+Shift+X.** xterm.js sends nothing for Ctrl+Shift with a letter (a terminal cannot
    tell it from Ctrl+letter), so no program in the terminal can be waiting for it: shells, vim,
    emacs, tmux (Ctrl+B), screen (Ctrl+A) and the agent CLIs bind Ctrl+letter, Alt+letter, Esc
    sequences and F-keys. Unlike Ctrl+Shift+Esc (Windows' task manager) or Shift+Esc (the
    browser's), no operating system takes it before the page. Firefox binds it to switch the
    text direction of a text field, and releasing prevents that. X reads as "leave", and it needs
    no F-key, which laptops hide behind Fn.
  - **F6** goes to the program in control mode: Midnight Commander, htop and others bind it, and
    a terminal that kept it back would break them. Leaving is the chord's job; once released, F6
    moves between the console's panes again, starting from the terminal.
  - **Paste** with Ctrl+Shift+V (Cmd+V on macOS) or the context menu's Paste, as in terminal
    emulators: xterm sends nothing for Ctrl+Shift+V, so the browser pastes, and xterm sends the
    text (bracketed when the program asks). Ctrl+V itself goes to the program (^V).
- **Always visible and announced.** The toolbar shows "Viewing" or "In control" with the key that
  changes it, and a polite live region says what the mode means as it changes. The program's end,
  or a stop, gives control back, and moves focus that was in xterm to the frame.
- **Focus ring.** The frame shows a ring whenever focus is in it: the accent in view mode, the
  warning colour in control mode.
- **Failures.** The terminal's chunk is under an error boundary: if it fails to load or render,
  the pane says so with a "Try again" that loads it afresh, and the rest of the console stays.
  A link to `?view=terminal` whose session cannot be loaded says why instead of loading for ever.

### Untrusted output

Output comes from an agent and from whatever it ran, so it is treated as hostile:

- **No clipboard writes from the program.** No clipboard addon is loaded, and OSC 52 is swallowed
  by a parser handler as well. Only the person's own Ctrl+C copies, and only their selection.
- **No titles.** OSC 0, 1 and 2 are swallowed; nothing listens to xterm's title event, so a title
  never reaches the document's.
- **No proposed API** (`allowProposedApi: false`), and every window report and manipulation
  (`windowOptions`) stays off, so a program cannot read back a title or the window's geometry.
- **Links** only for OSC 8 hyperlinks to absolute http and https URLs
  (`allowNonHttpProtocols: false`, checked again on activation), and only on Ctrl+click (Cmd+click
  on macOS) through the console's opener (`OpenExternalProvider`, else a new tab with no opener).
  A plain click does nothing; hovering shows the target. There is no web-links addon, so plain
  text is never a link.
- **Bounded memory, as far as it goes.** The scrollback keeps 5,000 lines: that bounds the number
  of rows, not every byte they hold, since a cell can carry any number of combining marks. xterm
  accepts OSC 8 link targets of up to 10 MB and keeps every link without an `id` with its line, so
  a target longer than 2 KiB is dropped (its text shows as plain text). What waits to be parsed is
  bounded by the flow control above.
- **No logging.** xterm's log level is off, so hostile sequences cannot flood the console.
- **No input in view mode**, not even the replies xterm generates to queries (device attributes,
  cursor position): the program hears from a viewer only in control mode.

### Rendering and accessibility

- **Renderer.** `@xterm/addon-webgl`; when WebGL 2 is missing or its context is lost, the addon is
  disposed and xterm's DOM renderer takes over. The frame says which (`data-renderer`).
- **Theme and font.** Background, ink, the accent, risk, ok and warn are the design tokens, read
  off the root element; the rest of the ANSI palette is chosen to read on the token background.
  `minimumContrastRatio: 4.5` corrects whatever colours a program asks for. A theme switch (or the
  system's) is picked up at once. The font is the mono token (`--pc-font-mono`) at `--pc-text-md`;
  xterm measures it once it has loaded (or after 1.5 s).
- **Dim text.** xterm draws SGR 2 dim text at half opacity and asks only half the minimum contrast
  of it, under 4.5:1 on the light background even for black. In the DOM renderer, generated rules
  draw dim text towards the background only as far as 4.5:1 allows. The WebGL renderer draws on
  a canvas that CSS cannot reach, so there dim text stays as xterm draws it.
- **Screen reader mode** (xterm's `screenReaderMode`), a checkbox in the toolbar, remembered per
  browser. With it on, xterm keeps the rows as text for screen readers (the Playwright specs read
  the screen through it).
- **Lifetime.** Unmounting disposes the socket, the resize, theme and colour-scheme observers, the
  WebGL addon and xterm.

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
  at the root as `src/main.tsx` does (`renderWithHub(…, { strict: true })`: a `<StrictMode>` under
  the `DataProvider` would not run effects twice). The terminal: `terminal-socket.test.ts` drives
  `TerminalSocket` and the diagnosis with a fake socket and fake timers (`terminal-fakes.ts`);
  `terminal-view.test.tsx` and `console-page.test.tsx` replace xterm with a fake, since happy-dom
  cannot draw one (the view's fake can hold back its write callbacks, for the flow control);
  `terminal-options.test.ts` covers keys, links, the theme and dim text;
  `terminal-boundary.test.tsx` and `work-boundary.test.tsx` their error boundaries. The Work view
  needs no `tz`: it shows `SessionWork`'s blocks (`useRecapBlocks`), not day paragraphs, and the
  mock hub's blocks endpoint takes no `tz` at all (only `GET /v1/recaps/days` does).
- **The workbench.** `workbench-layout.test.ts` tests the model and its storage (round trips, every
  kind of damage refused, storage that throws); `workbench.test.tsx` the workbench in the console
  (preview tabs, splits with a view per pane, the URL, a reload restoring the layout and a corrupt
  one ignored, the keys and their palette commands, the details and files with an unsaved edit
  across tabs, dragging tabs). Its tests reset the per-page stores (`resetWorkbenchStores`,
  `clearDrafts`) after each test, as `console-page.test.tsx` does.
- **Playwright** (`src/console/tests/e2e`): the acceptance run in the real app against the mock
  hub, axe included (both themes, both layouts), on ports 47450 (hub) and 47451 (UI), which
  `E2E_HUB_PORT` and `E2E_UI_PORT` move. Traces are kept only for failures, in
  `apps/ui/test-results/console`. `terminal.spec.ts` reads the screen through xterm's screen reader
  rows (WebGL draws on a canvas), and forces the DOM renderer once per theme. The demo workspace
  has no session with a terminal on the unreachable machine, so the 503 spec tells the browser
  that SES0005 has one (`page.route`); the hub still refuses its socket with a 503. For the same
  reason as the Vitest suite, this config's browser is not pinned to UTC (compare
  `src/projects/tests/e2e/playwright.config.ts`, whose Summary days need `tz=0`): the Work view's
  blocks need no `tz`.

  ```sh
  cd apps/ui
  PLAYWRIGHT_CHANNEL=msedge corepack pnpm exec playwright test -c src/console/tests/e2e/playwright.config.ts
  ```

  `workbench.spec.ts` opens a session, splits it, switches the copy to its terminal, opens the
  details and a coloured file and a PDF from its folders, drags tabs into a strip and onto a
  pane's edge, checks axe in both schemes (`e2e/axe.ts`), and reloads to the same layout; then the
  keys and the palette, and a corrupt stored layout. It seeds its two files through the mock hub's
  files API and finds the hub from the app's own requests, so it runs under this config and under
  the UI's own (CI's), which imports it through `e2e/workbench.spec.ts`.

## Link a session

Right-click a session row (or use its header's Actions menu) and choose “Link to…”. Choose a
workstream and optionally one of its tasks. Changing workstream clears the task; errors keep the
dialog open for retry. The mutation leaves the cache to the existing `session_linked` stream
invalidation, so sessions move out of Unsorted without a reload. `link-session.test.tsx` tests
selection and retry; `tests/e2e/link-session.spec.ts` covers the row action and axe checks. The
root e2e suite imports that spec through a one-line discovery shim.

Run the three entry points against a disposable real daemon (never a real agent CLI):

```sh
cargo build -p pitcrew-daemon -p pitcrew-ptyd --bins --locked
node apps/ui/src/console/tests/e2e/run-real.mjs
```

In Codex cloud source `/workspace/.onboarding/env.sh` before building. Install the frozen UI
dependencies and Playwright Chromium during setup. The runner works on Linux, macOS and Windows,
creates temporary homes and a synthetic Claude shim, and removes its own processes and files.
`PITCREW_SESSION_UI_CONFIG` can point at a Playwright config using an installed browser.
