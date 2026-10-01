# Brief L · The data layer through the desktop gateway

- **Stream:** L · UI foundation. **Branch:** `s/L/desktop-transport`. **Paths:**
  `apps/ui/src/data/**`, `apps/ui/src/shell/**`, `apps/ui/package.json` (+ `pnpm-lock.yaml`).
- **First read:** [README.md](README.md), [the desktop gateway contract](../contracts/desktop-gateway.md),
  `apps/ui/src/data/README.md`, `api.ts`, `stream.ts`, `config.ts` and `provider.tsx`, and
  `apps/ui/src/shell/README.md` (the workspace switcher).

## Goal

In the desktop app, every request and socket from the UI goes through the gateway, and the UI
holds no token. In the browser, nothing changes.

## What to build

1. **A transport seam** in `src/data`:
   - `request(method, path, body) → { status, contentType, body }`;
   - `openSocket(path) → SocketLike`, the interface `stream.ts` already uses.
   - `createApi` and `StreamClient` take a transport instead of a base URL and token.
   - **Browser transport:** today's `fetch` and `WebSocket`, with the bearer token in development.
   - **Desktop transport:** the gateway's commands and channels, exactly as the contract says.
     - A `GatewayError` becomes an `ApiError`-like error the UI already handles: `unreachable`
       becomes the same state as a network failure, and `needs_pairing` its own state.
     - Binary channel messages arrive as `ArrayBuffer`.
     - `close` ends the `SocketLike`, with the code.
   - The transport is chosen once at start: the desktop when `window.__TAURI_INTERNALS__`
     exists, the browser otherwise. Nothing reads `VITE_PITCREW_API` or a token in the desktop.
2. **Workspaces:**
   - In the desktop, the shell's workspace switcher lists `gateway_workspaces()` and follows the
     `gateway://workspaces` event.
   - Each workspace's queries are kept apart, so switching never shows another workspace's
     data. Either use one `QueryClient` per workspace, or put the workspace in every key; say
     which, and why.
   - A workspace that is `unreachable` or `needs_pairing` shows that state, not a spinner.
3. **For other streams:**
   - Export `openSocket` (or a `useSocketFactory()` hook) from `src/data`, so the console's
     terminal (stream M) can open its socket through the same transport. Document it in the
     data README.
   - Don't edit `src/console`. Stream M will switch to it.
4. **Dependency:** `@tauri-apps/api`, pinned exactly (MIT or Apache-2.0).
   - Load it only in the desktop, by dynamic import, so the browser's first chunk doesn't grow.
   - Check the build output and `pnpm size`.

## Acceptance

- **Vitest**, with `@tauri-apps/api/mocks` (`mockIPC`) as a fake gateway that follows the
  contract:
  - requests, including an `ApiError` status passed through;
  - each `GatewayError` code;
  - stream and socket messages in order, binary as `ArrayBuffer`, and `close` last;
  - reconnect after a 1013 close, with `since`;
  - workspace switching keeps the data apart;
  - the `gateway://workspaces` updates.
- In the desktop transport, no token, `Authorization` or `VITE_PITCREW_*` value is ever read or
  sent. A test checks this.
- The browser e2e suites (shell, console, projects, onboarding) still pass unchanged.
- Typecheck, lint, the tests, the build and `pnpm size` all pass.

## Out of scope

The gateway itself (stream K), pairing and adding workspaces (stream O), and the console's
terminal socket (stream M switches to your export after this merges).
