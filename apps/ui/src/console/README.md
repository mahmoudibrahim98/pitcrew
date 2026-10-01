# console (stream M)

The Agent console. See `docs/build/streams/M.md`. `index.ts` registers it with the shell (`feature`)
and exports its components. The app imports `index.ts` at start, so it stays small: the page and
every component are lazy chunks (render the components inside a `<Suspense>`).

| File | What |
|---|---|
| `index.ts` | `feature`: the routes `console` and `console/$session` (one lazy page for both) and the palette commands. The lazy component exports. |
| `console-page.tsx` | `ConsolePage`: filters, list and session side by side, or one at a time when narrow. See [The page](#the-page). |
| `search.ts` | The facets in the URL's search (`?machine=A,B&state=waiting`), and the session pane's view (`?view=terminal`). No React. |
| `view-switch.tsx` | `ViewSwitch`: the session pane's Chat \| Terminal switch; without a terminal, Terminal stays focusable but does nothing, and the reason shows beside it. |
| `terminal/` | The terminal, a lazy chunk with xterm.js in it. See [The terminal](#the-terminal). |
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
- **Chat or terminal.** Under the header, a Chat | Terminal switch. The terminal is in the URL's
  search (`?view=terminal`, replacing the history entry), so a link or a reload shows it again;
  choosing another session keeps it. A session without a `terminal` shows the chat, and the
  switch's Terminal option is disabled with the reason ("This session has no terminal."). In a
  narrow console the terminal takes the session pane: the header keeps only its title row.
- **Keys.** F6 and Shift+F6 move between the panes: the filters, the list, then the transcript and
  the composer, or the terminal in their place (those on screen). In the list, the arrows, Page
  keys, Home and End choose the session beside it once they rest on one (150 ms); Enter or Space
  opens it and goes to the composer. Only a terminal in control mode claims the shell's keys
  (`ownsShellKeys`), and it keeps F6 too. In the composer, Ctrl B, Ctrl J and Ctrl . stay with the
  text field, and Ctrl K opens the palette as everywhere.
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

## The terminal

A session with a `terminal` shows it live: xterm.js on `GET /v1/sessions/{id}/terminal`
(`docs/build/contracts/api-v1.md`, "Terminals"). `TerminalView` is a lazy chunk, and xterm is in
that chunk alone: nothing of it loads until a terminal is on screen (`index.ts` also exports it,
lazily, for the workbench).

| File | What |
|---|---|
| `terminal/socket.ts` | `TerminalSocket`, no React: the connection, reconnects by offset, keystrokes and resizes (below). `browserSocketFactory` opens browser WebSockets. |
| `terminal/diagnose.ts` | Why the hub refused a terminal, asked over HTTP (below). |
| `terminal/controller.ts` | `TerminalController`, no React: xterm, its addons, fit, theme and font, input by mode, flow control, and disposal. |
| `terminal/terminal-view.tsx` | `TerminalView`: the mode, Take control and Release, the status, the screen reader setting, the truncated marker, and the frame xterm draws in. |
| `terminal/options.ts` | xterm's options for hostile output, and the link handler. |
| `terminal/keys.ts` | The release chord and view mode's keys. |
| `terminal/theme.ts`, `terminal/contrast.ts` | Colours and font from the design tokens; dim text that keeps 4.5:1. |
| `terminal/prefs.ts` | The screen reader mode, per browser (`pitcrew.terminal`). |

### The connection

- **Sockets by path.** `TerminalSocket` asks a factory for an API path
  (`/v1/sessions/{id}/terminal?cols=&rows=&from=`), never a URL. `browserSocketFactory` puts it on
  the hub's base (`ws:` or `wss:`, a path prefix kept) and offers the subprotocols `pitcrew.v1` and
  `pitcrew.bearer.<token>`. **The token never goes in the URL.** The desktop app will pass its
  gateway's opener instead (`docs/build/contracts/desktop-gateway.md`), through `TerminalView`'s
  `socket` prop.
- **No byte lost or repeated.** It counts the output bytes received and reconnects with
  `from=<count>`. `{"type":"truncated","from":N}` moves the count to N (the hub no longer has the
  bytes between), and the view shows "Earlier output is no longer available." as a line at the top
  of the terminal. The marker is page text, not bytes written into the terminal: the program's
  screen is left as it drew it, and a clear-screen cannot erase the marker.
- **The end.** `{"type":"exit"}` or a 1000 close: "The program ended." It never reconnects.
- **Close codes.** 1007, 1009 and 1011 (and 1002, 1003, 1008) stop with the reason, the hub's
  close reason after it. 1001, 1013, 1006 and anything else reconnect.
- **A refused upgrade.** A browser shows every refused upgrade as a 1006 close before `open`; the
  HTTP status is hidden. So after one, `terminalDiagnosis` asks the hub as it decides: the session
  (404: "This session no longer exists."), its machine (503: "gpu-box cannot be reached right now,
  so its terminal cannot be shown."), its terminal (404: "This session has no terminal."). Any of
  those stops it, with the reason; if none holds, the network failed, and it reconnects. A
  transport that knows the status (the desktop gateway) passes it in the close event, and 400,
  401, 403, 404 and 503 stop without asking.
- **Back-off.** Attempt n waits `min(15 s, 500 ms × 2ⁿ)` times 0.5–1 (jitter), and starts over
  once a connection has stayed up for 5 s. While the page is hidden or the browser offline it does
  not reconnect at all; it reconnects at once when the page is visible and online again.
- **Keystrokes** are binary frames (UTF-8 from `TextEncoder`; X10 mouse reports byte for byte).
  While disconnected they wait, 64 KiB at most in all; past that, or more than 64 KiB at once (a
  large paste), or more than 64 KiB not yet sent on a slow connection, they are refused and the
  view says so. Nothing is buffered without limit, and no message can reach the hub's 1 MiB limit.
- **Resize.** xterm is fitted to its element (`@xterm/addon-fit` and a `ResizeObserver`); the size
  goes to the hub 100 ms after it settles, clamped to 1..=1000, and only when it changed. The first
  connection already carries the fitted size.
- **Flow control.** Output given to xterm and not yet parsed is counted. Past 4 MiB the socket
  holds (closes) and reconnects from the bytes received once xterm is down to 512 KiB, so a
  flood costs neither unbounded memory nor output.

### View and control

- **View mode** is the default. xterm's input is off (`disableStdin`) and it acts on no key, and
  focus stays on the terminal's frame (never in xterm's text field), so every shell shortcut works
  as anywhere else. Enter takes control; the arrows, Page Up, Page Down, Home and End scroll; Ctrl+C
  (Cmd+C on macOS) copies a selection.
- **Control mode** is taken with "Take control" or Enter on the focused terminal. The frame then
  spreads `ownsShellKeys`, and every key goes to the program: Esc, Tab, Ctrl K, J, B and . included.
  It is left with "Release" or **Ctrl+Shift+X** (Ctrl, not Cmd, on every platform, as terminal keys
  are), which never reaches the program.
  - **Why Ctrl+Shift+X.** xterm.js sends nothing for Ctrl+Shift with a letter (a terminal cannot
    tell it from Ctrl+letter), so no program in the terminal can be waiting for it: shells, vim,
    emacs, tmux (Ctrl+B), screen (Ctrl+A) and the agent CLIs bind Ctrl+letter, Alt+letter, Esc
    sequences and F-keys. Unlike Ctrl+Shift+Esc (Windows' task manager) or Shift+Esc (the
    browser's), no operating system or browser takes it before the page; Firefox's text-direction
    switch on it is prevented. X reads as "leave", and it needs no F-key, which laptops hide
    behind Fn.
  - **F6** goes to the program in control mode: Midnight Commander, htop and others bind it, and
    a terminal that kept it back would break them. Leaving is the chord's job; once released, F6
    moves between the console's panes again, starting from the terminal.
- **Always visible and announced.** The toolbar shows "Viewing" or "In control" with the key that
  changes it, and a polite live region says what the mode means as it changes. The program's end,
  or a stop, gives control back.
- **Focus ring.** The frame shows a ring whenever focus is in it: the accent in view mode, the
  warning colour in control mode.

### Untrusted output

Output comes from an agent and from whatever it ran, so it is treated as hostile:

- **No clipboard writes from the program.** No clipboard addon is loaded, and OSC 52 is swallowed
  by a parser handler as well. Only the person's own Ctrl+C copies, and only their selection.
- **No titles.** OSC 0, 1 and 2 are swallowed; nothing listens to xterm's title event, so a title
  never reaches the document's.
- **No proposed API** (`allowProposedApi: false`), and every window report and manipulation
  (`windowOptions`) stays off, so a program cannot read back a title or the window's geometry.
- **Links** only for OSC 8 hyperlinks to absolute http and https URLs
  (`allowNonHttpProtocols: false`, checked again on activation), and only on Ctrl+click (Cmd+click)
  through the console's opener (`OpenExternalProvider`, else a new tab with no opener). A plain
  click does nothing; hovering shows the target. There is no web-links addon, so plain text is
  never a link.
- **Bounded scrollback: 5,000 lines.**
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
  as `src/main.tsx` does. The terminal: `terminal-socket.test.ts` drives `TerminalSocket` with a
  fake socket and fake timers (`terminal-fakes.ts`); `terminal-view.test.tsx` and
  `console-page.test.tsx` replace xterm with a fake, since happy-dom cannot draw one;
  `terminal-options.test.ts` covers keys, links, the theme and dim text.
- **Playwright** (`src/console/tests/e2e`): the acceptance run in the real app against the mock
  hub, axe included (both themes, both layouts), on ports 47450 (hub) and 47451 (UI), which
  `E2E_HUB_PORT` and `E2E_UI_PORT` move. Traces are kept only for failures, in
  `apps/ui/test-results/console`. `terminal.spec.ts` reads the screen through xterm's screen reader
  rows (WebGL draws on a canvas), and forces the DOM renderer once per theme. The demo workspace
  has no session with a terminal on the unreachable machine, so the 503 spec tells the browser
  that SES0005 has one (`page.route`); the hub still refuses its socket with a 503.

  ```sh
  cd apps/ui
  PLAYWRIGHT_CHANNEL=msedge corepack pnpm exec playwright test -c src/console/tests/e2e/playwright.config.ts
  ```
