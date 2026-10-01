# Brief K · The Tauri app, the gateway, and the local daemon

- **Stream:** K · Desktop shell. **Branch:** `s/K/shell-and-gateway`. **Paths:** `apps/desktop/**`
  (+ `pnpm-lock.yaml` if you add the Tauri CLI).
- **First read:** [README.md](README.md), `docs/build/streams/K.md`, ADR-0003, ADR-0006, ADR-0009,
  [the desktop gateway contract](../contracts/desktop-gateway.md),
  [API v1](../contracts/api-v1.md) ("Transport and auth", "Live updates", "Terminals"), and
  `crates/daemon/README.md` (`pitcrewd serve --listen private`, `token show-path`).

## Goal

The desktop app starts, starts or finds the person's local `pitcrewd`, and serves the UI. The UI
reaches the daemon only through the gateway, and the webview never sees a token.

## Your workspace

`apps/desktop/src-tauri` is **its own Cargo workspace** (the root `Cargo.toml` excludes it), with
its own `Cargo.lock`. Build it with
`cargo clippy --manifest-path apps/desktop/src-tauri/Cargo.toml --all-targets --locked`.

- Keep the lints and package fields in its `Cargo.toml` in step with the root's.
- Depend on `pitcrew-protocol` and `pitcrew-interfaces` by path, never on another stream's
  internals.
- CI already has a desktop job, with the WebKitGTK packages installed on Linux.

## What to build

1. **The Tauri 2 app:**
   - It loads `apps/ui`: the Vite dev server in development, and `apps/ui/dist` in release
     builds.
   - **A strict CSP.** No inline scripts, no remote origins, and `connect-src` only for Tauri's
     IPC. If the UI needs `style-src 'unsafe-inline'`, show why.
   - **Minimal capabilities.** The main window gets only the gateway commands and the
     `gateway://workspaces` event. No shell, fs, http or opener plugins.
   - No devtools in release builds; one main window; the single-instance plugin.
   - Never load a remote URL. Refuse navigation away from the app's own origin, and open
     external links through a gateway command later (not now).
2. **The gateway**, exactly as the contract says:
   - `gateway_workspaces`, `gateway_request`, and `gateway_socket_open`, `_send` and `_close`;
   - the path checks, the size limits and the response allow-list;
   - the error codes and the close codes;
   - back-pressure at 8 MiB, and cleanup when a window reloads or closes.

   It talks HTTP and WebSocket to the daemon over the daemon's private socket: a unix socket, or
   a named pipe on Windows. Use hyper 1 for HTTP and tokio-tungstenite `client_async` for
   WebSockets over that stream.
3. **The local daemon:**
   - Find `pitcrewd`: a configured path, next to the app's executable, or `PATH`, in that order.
   - Use the daemon if it is already running for this user. If not, start
     `pitcrewd serve --listen private` and supervise it: restart with a bounded backoff, show it
     as `unreachable` after repeated failures, and stop it when the app quits, but only if the
     app started it.
   - Read its device token from the file that `pitcrewd token show-path` names, each time you
     connect. Never copy it, never log it, and never send it to the webview.
4. **The workspace registry:**
   - A JSON file in the app's data directory, written atomically with private permissions.
   - On first start, register the local workspace, with its id and name from
     `GET /v1/workspace`.
   - Emit `gateway://workspaces` when a workspace's state changes.
   - Remote workspaces come in the next brief. Leave room for them: `kind`, and a connection
     enum.
5. **A keychain store:** `TokenStore`, with an OS keychain implementation (`keyring`) and an
   in-memory one for tests. Nothing uses it yet besides the tests; remote pairing will.

## Acceptance

- **Tests (Rust):**
  - path validation, as a table (`..`, `//`, `\`, `#`, control characters, non-`/v1/`);
  - the body and response size limits, and the response header allow-list;
  - error mapping: daemon down, 503 on upgrade, unknown workspace;
  - against a fake daemon on a unix socket in the test (axum as a dev-dependency):
    - socket message order, ending with `close`;
    - 1006 on a broken connection;
    - 1009 on an oversize send;
    - 1013 under back-pressure;
    - Pings answered by the gateway.
  - Cleanup on window close.
  - **No token reaches the webview:** run every command and event against the fake daemon with a
    known token, and search every result, event, error and log line for it.
  - The supervisor, with a fake `pitcrewd` script: start, restart with backoff, give up, stop
    only what it started.
- **Run the app** in WSLg against `pitcrewd serve --demo`, and attach a screenshot. If stream L's
  desktop transport isn't merged yet, show the gateway working from the UI's devtools in a debug
  build instead: one `gateway_request` and one stream socket.
- Measure cold start to the first painted UI, and idle memory. Report both; the 1.5 s and 300 MB
  goals are not enforced yet.
- fmt, clippy (Linux), tests, and `cargo deny check` on this manifest all pass.
- **Windows target:** try `cargo-zigbuild clippy --target x86_64-pc-windows-gnu` on this
  manifest. Report what happens; it may not be possible without a Windows toolchain.

## Out of scope

Remote workspaces and the SSH tunnel, pairing, the tray, notifications, deep links, the updater,
installers (stream P), and the UI side (stream L).
