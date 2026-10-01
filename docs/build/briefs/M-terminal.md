# Brief M · The terminal: xterm.js on the terminal WebSocket

- **Stream:** M · UI: Agent console. **Branch:** `s/M/terminal`. **Paths:** `apps/ui/src/console/**`.
- **First read:** [README.md](README.md), [M-console-wiring.md](M-console-wiring.md) (merged),
  `docs/build/streams/M.md` (work package 4), `docs/build/contracts/api-v1.md` (authentication
  for WebSockets, and "Terminals"), `apps/ui/src/console/README.md`, `apps/ui/src/shell/README.md`
  (`ownsShellKeys`, keys), `apps/ui/src/data/README.md` and `stream.ts` (how the live stream
  socket is opened and reconnects), and `apps/mock-hub/README.md` ("Terminals").

## Goal

A session that has a terminal shows it, live, in the console. A person can watch it, or take
control and type into it. A dropped connection comes back without losing or repeating output.

## What to build

1. **`TerminalSocket`** (no React), in `src/console/terminal/`:
   - Opens `GET /v1/sessions/{id}/terminal?cols=&rows=&from=` on `ws:`/`wss:` from
     `apiBaseUrl`. Its subprotocols are `pitcrew.v1` plus `pitcrew.bearer.<token>` when there is
     a token, as `stream.ts` does. **The token never goes in the URL.** Use a socket factory, as
     `stream.ts` does, so tests can inject a fake.
   - Counts the output bytes it has received, and reconnects with `from=<count>`.
   - `truncated`: the count jumps to its `from`. Show a one-line marker in the terminal
     ("Earlier output is no longer available").
   - `exit`, or a 1000 close: stop and show that the program ended. Never reconnect.
   - 1013, 1001 and abnormal closes: reconnect with a bounded, jittered backoff. Pause while the
     page is hidden or offline, as the live stream does.
   - 1007, 1009, 1011 and an HTTP 503 or 404 before the upgrade: stop, and show why. Never loop.
   - Keystrokes are binary frames (`TextEncoder`). While disconnected, keep at most 64 KiB of
     them; past that, refuse the input and say so. Never buffer without limit.
   - Resize: debounce it, clamp to 1..=1000, and send it only when the size changed.
2. **`TerminalView`**, a lazy component:
   - **Rendering.** `@xterm/xterm` 6 with `@xterm/addon-webgl`. When WebGL is unavailable or its
     context is lost, fall back to xterm's DOM renderer.
     - Fit with `@xterm/addon-fit` and a `ResizeObserver`.
     - Take colours and font from the design tokens, and follow theme changes.
     - Dispose everything on unmount (the WebGL context, observers, the socket).
   - **Lazy loading.** xterm loads only when a terminal is shown. Check the build output, and
     keep `pnpm size` within budget. If the budget can't hold xterm's chunk, say so; don't raise
     it yourself.
3. **View and control modes:**
   - **View mode** is the default. Input is off, and the shell keeps all its keys.
   - **Control mode** is entered explicitly, with a "Take control" button or Enter on the
     focused terminal. The container then spreads `ownsShellKeys`, so Ctrl K, J and B reach the
     program. Leaving it takes a visible "Release" button and one documented key chord. Pick the
     chord, justify it, and make sure it doesn't collide with common terminal programs. Esc must
     reach the program.
   - The current mode is always visible and announced through a live region.
   - Decide what F6 does in control mode, and document it.
4. **Placement.**
   - When the session has a `terminal`, the session pane gets a Chat | Terminal switch, kept in
     the URL search (`?view=terminal`) so a link or reload reproduces it.
   - Without a terminal, the switch is disabled with the reason.
   - On a narrow layout, the terminal takes the session pane.
5. **Untrusted output.** Terminal output comes from an agent and from whatever it ran, so treat
   it as hostile:
   - no clipboard writes from the program (no OSC 52; don't add a clipboard addon);
   - title changes (OSC 0 and 2) never reach the document title;
   - no proposed API (`allowProposedApi: false`);
   - links only if they are http or https, opened through the console's `OpenExternalProvider`
     path, and only on a modifier-click; without that, no links at all;
   - a bounded scrollback (state the number).
   Document each choice in the console README.
6. **Accessibility.**
   - A screen-reader mode toggle (xterm's `screenReaderMode`), remembered per browser.
   - A focus ring on the terminal container.
   - axe passes.

## Acceptance

- **Vitest**, with a fake socket, for `TerminalSocket`:
  - output counted across reconnects, with no bytes lost or repeated;
  - `truncated` at the start and in the middle;
  - `exit`, then no reconnect;
  - each close code's behaviour, and backoff bounds;
  - the 64 KiB keystroke cap;
  - resize debounce and clamping;
  - the token only in the subprotocols, never in the URL.
- **Playwright** against the mock hub (own ports, `PLAYWRIGHT_CHANNEL=msedge`):
  - open a session with a terminal and see the canned screen;
  - take control, type, and see the echo;
  - release, and Ctrl K opens the palette again;
  - a session that ran over a day shows the truncated marker;
  - a session on the unreachable machine shows the 503 reason;
  - reload with `?view=terminal` and the screen comes back once, not twice;
  - axe in both themes and both layouts.
- No xterm code loads before a terminal is shown (check the build output).
- Typecheck, lint, the tests and the build pass; `pnpm size` stays within budget.

## Out of scope

The workbench tabs and splits, the file viewer, starting a session, the runner link (D), and
changes to the mock hub (if you need one, describe it in your report).
