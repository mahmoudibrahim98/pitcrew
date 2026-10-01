# Brief L · Desktop navigation, a shared popover, and socket back-pressure

- **Stream:** L · UI foundation. **Branch:** `s/L/desktop-polish`. **Paths:** `apps/ui/src/data/**`,
  `apps/ui/src/shell/**`, `apps/ui/src/design/**`, `apps/ui/*`, `apps/ui/tests/**`.
- **First read:** [README.md](README.md), [the desktop gateway contract](../contracts/desktop-gateway.md)
  ("Navigation from outside the window": `gateway://navigate` and `NavigateTarget`), the
  `src/data`, `src/shell` and `src/design` READMEs, and `apps/ui/src/projects/recap-text.tsx`
  (the popover the Projects layout built for recap evidence).

## Goal

The desktop app's deep links and notifications open the right place. Features share one
accessible popover. A feature's socket can tell when it is sending faster than the connection
takes.

## What to build

1. **`gateway://navigate`,** in the desktop only:
   - listen for the event and validate the `NavigateTarget` (the workspace is a known ULID, the
     kind is one of the five, `id` is required unless the kind is `inbox`);
   - map it to the app's routes with the shell's path helpers (`paths.inbox`, `paths.task`,
     `paths.session`, `paths.project`, `paths.workstreamById`), and navigate;
   - an unknown workspace goes to `/` with a notice; anything invalid is dropped and logged;
   - never treat any field as a URL or a path to concatenate;
   - test it with `mockIPC`.
2. **A shared popover** in `src/design`: a non-modal popover for evidence and details, from the
   one in `projects/recap-text.tsx`.
   - It can render inline, or in a portal.
   - It has the keyboard behaviour that one has: activation moves focus in, Escape returns it,
     and an inert preview on focus.
   - Document it, and test it with axe. Don't edit `src/projects`; stream N will switch to it.
3. **Socket back-pressure:**
   - Add `bufferedAmount` to `TransportSocket` (browser: the WebSocket's; desktop: the bytes
     queued in the gateway transport's outbox).
   - Bound the desktop transport's outbox, closing with 1013 when a sender ignores it, as the
     server does.
   - The console's terminal already checks it duck-typed, so make the type real. Test both
     transports.
4. **Dev safety:** `strictPort: true` in Vite's dev server. If the port is taken, the dev desktop
   then fails instead of loading whatever answers on the next port. Say so in the README.
5. **Background streams:** cap the streams kept for workspaces that are not in view. Close a
   workspace's stream after 10 minutes unfocused, and resume with `since` when it comes back.
   Test it with fake timers.

## Acceptance

- Vitest covers each item above. The browser e2e suites (shell, console, projects, onboarding)
  pass unchanged.
- Typecheck, lint, the tests, the build and `pnpm size` all pass. The initial JS may not grow by
  more than 1 kB gzipped: keep the popover lazy where features are.

## Out of scope

Stream N's switch to the shared popover, routes for event and file receipts (a product decision
still to make), and pairing.
