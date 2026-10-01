# pitcrew-desktop

The PitCrew desktop app (ADR-0003): a Tauri 2 shell around the UI in `apps/ui`, the **gateway**
that is the webview's only way to a workspace's daemon, and the supervisor of the person's local
`pitcrewd`. All logic stays in `pitcrewd`.

**Owned by stream K**: see [docs/build/streams/K.md](../../../docs/build/streams/K.md) and the
[gateway contract](../../../docs/build/contracts/desktop-gateway.md).

This directory is its own Cargo workspace (the root `Cargo.toml` excludes it), with its own
`Cargo.lock`:

```bash
cargo clippy --manifest-path apps/desktop/src-tauri/Cargo.toml --all-targets --locked -- -D warnings
cargo test --manifest-path apps/desktop/src-tauri/Cargo.toml --locked
```

## Running it

**Development** (debug build, Vite dev server, devtools):

```bash
corepack pnpm --filter @pitcrew/ui dev        # serves http://127.0.0.1:5173 (devUrl)
cargo run --manifest-path apps/desktop/src-tauri/Cargo.toml
```

**Release** (the built UI inside the app):

```bash
corepack pnpm --filter @pitcrew/ui build      # apps/ui/dist (frontendDist)
cargo build --manifest-path apps/desktop/src-tauri/Cargo.toml --release --features custom-protocol
```

A release build without `custom-protocol` does not compile, so a release never loads the dev
server. `TAURI_CONFIG='{"build":{"frontendDist":"…"}}'` bundles a UI built elsewhere.

**Settings.** `settings.json` in the app's config directory (`~/.config/org.pitcrew.desktop`,
`%APPDATA%\org.pitcrew.desktop`, `~/Library/Application Support/org.pitcrew.desktop`), all
optional; `PITCREW_PITCREWD` and `PITCREW_STATE_DIR` override them:

```json
{ "pitcrewd": "/opt/pitcrew/bin/pitcrewd", "stateDir": "/home/sam/.local/share/pitcrew" }
```

Without `stateDir` the app uses the daemon's own default, so it finds a `pitcrewd` started by
hand. Logs go to stderr at `PITCREW_DESKTOP_LOG` (default `info`).

## The local daemon (`src/daemon`)

- **Finding it:** the configured path, then `pitcrewd` next to the app's executable, then `PATH`
  (absolute entries only). A configured path that is not a program is an error.
- **Find or start:** if a daemon already answers `GET /v1/host/info` on this user's private
  socket or pipe, the app uses it and never stops it. Otherwise it starts
  `pitcrewd [--state-dir <dir>] serve --listen private` and waits up to 20 s for
  `pitcrewd listening on …`.
- **Trust before the token:** every connection is checked with `pitcrew_api::client` first: on
  Unix the socket's directory is ours and 0700, the socket is ours, and the peer runs as us; on
  Windows the pipe is opened at identification level and its owner must be us.
- **The token:** `pitcrewd token show-path` names the file; the app keeps the path and reads the
  token **on every connection**. It never copies, logs or returns it.
- **Supervision:** a daemon the app started that stops is restarted after a wait that doubles
  from 0.5 s (at most 15 s). Five failures in a row (runs shorter than a minute, or starts that
  never got ready) give up: the workspace is `unreachable` with the daemon's last words, until the
  app restarts. A run of a minute or more starts the count and the wait afresh.
- **Someone else's daemon** is checked every 5 s (or at once when a request fails); if it goes
  away, the app starts its own.
- **Quitting:** the app stops the daemon only if it started it: SIGTERM and up to 8 s on Unix,
  then a kill. Windows has no signal one process can send another without a shared console, so
  there the daemon is terminated (SQLite's WAL keeps the store consistent).

## The gateway (`src/gateway`, `src/commands.rs`)

Exactly the [contract](../../../docs/build/contracts/desktop-gateway.md):

```js
invoke('gateway_workspaces')                                   // → GatewayWorkspace[]
invoke('gateway_request', { req: { workspace, method, path, body } })  // → { status, contentType?, body }
invoke('gateway_socket_open', { workspace, path, events: channel })    // → { socket }
invoke('gateway_socket_send', { socket, text })                // or { socket, binary }
invoke('gateway_socket_close', { socket, code, reason })
```

- Every argument is checked here; anything malformed is `invalid`. Paths: `/v1/…` only, no `.`
  or `..` segment (also percent-encoded), no `//`, `\`, `#`, control characters or raw non-ASCII;
  the query goes on as it is. Sockets: `/v1/stream` and `/v1/sessions/{id}/terminal` only.
- Limits: 1 MiB request bodies, 32 MiB response bodies, 1 MiB (terminal) and 4 KiB (stream)
  client messages, 8 MiB incoming frames per message.
- The gateway sends `Host`, `Content-Type` and `Authorization` (marked sensitive); the WebSocket
  carries `Sec-WebSocket-Protocol: pitcrew.v1, pitcrew.bearer.<token>`. Only the status, the
  `Content-Type` and the body come back.
- Text and close messages go on the channel as JSON; binary frames as raw IPC bodies
  (`ArrayBuffer`). `close` is always last, also after the webview's own close; then the channel is
  dropped, which ends it on the webview's side.
- **Back-pressure.** Each frame counts against an 8 MiB budget until the webview has taken it.
  Tauri's channels give no acknowledgement, so the gateway sends a probe (`eval_with_callback`
  of a no-op) after the frames: the webview runs scripts in order, so when the probe comes back,
  everything before it has been handed to the page. One probe is in flight at a time. Past the
  budget the socket closes with 1013. Large frames go through Tauri's fetch path; a probe can
  count them as taken just before the page has fetched them, which is the only slack.
- **Pings** are answered by tungstenite as it reads; neither Ping nor Pong reaches the webview.
- **Cleanup.** A socket belongs to the webview that opened it. When that page starts loading again
  (a reload) or its window is destroyed, its sockets close with 1001 and nothing more is sent to
  it. A socket that finishes opening after its page reloaded is closed at once.

## The window, the CSP and the capability (`src/app.rs`, `tauri.conf.json`, `capabilities/`)

- One window, `main`, created in code with its guards: navigation away from the app's origin
  (`tauri://localhost`, `http://tauri.localhost` on Windows, or the dev server in debug builds) is
  refused, new windows and downloads are denied, devtools exist only in debug builds. The
  single-instance plugin focuses it on a second launch.
- **The capability** (`capabilities/main.json`) gives `main` the five gateway commands and
  `core:event:allow-listen`/`allow-unlisten`, nothing else: no `core:default`, no shell, fs, http
  or opener plugin, no emitting events. `build.rs` declares the commands in the app's ACL
  manifest, so no command runs without a capability naming it. The default `dynamic-acl` feature
  is off, so none can be added at run time. Tauri 2.12 cannot scope `listen` to one event name;
  the app emits only `gateway://workspaces`.
- **The CSP:**

  | Directive | Value | Why |
  |---|---|---|
  | `default-src` | `'self'` | Only the bundled UI. |
  | `script-src` | `'self'` | No inline scripts, no eval, no remote scripts. |
  | `style-src` | `'self' 'unsafe-inline'` | See below. |
  | `img-src`, `font-src` | `'self'` | Fonts and icons are bundled files (`assetsInlineLimit: 0`). |
  | `connect-src` | `ipc: http://ipc.localhost` | Tauri's IPC only: the webview cannot reach a daemon, or anything else, itself. |
  | `object-src`, `frame-src`, `worker-src`, `media-src`, `manifest-src` | `'none'` | Not used. |
  | `base-uri`, `form-action`, `frame-ancestors` | `'none'` | No base rewriting, no form posts, no framing. |

  `style-src 'unsafe-inline'`: two of the UI's libraries create `<style>` elements at run time,
  with CSS computed then, so neither hashes nor a nonce can cover them:
  `react-remove-scroll-bar` (through Radix's modal Dialog, Popover and Menu: the scroll lock), and
  xterm.js (the console's terminal: its theme and cell sizes). Measured in WebKitGTK with
  `style-src 'self'`: opening the command palette reports a `style-src-elem` violation for an
  inline style from the app's own bundle. Styles cannot run code, and with `img-src`, `font-src`
  and `connect-src` closed, injected CSS cannot send anything out.
- `freezePrototype` is on.

## The registry and the keychain

- `workspaces.json` in the app's **local** data directory (never a roaming one), written
  atomically (a new private file, mode 600 on Unix, renamed over the old) with
  `{ "version": 1, "workspaces": [{ "id", "name", "kind", "connection": { "type": "local" } }] }`.
  States live in memory; every change emits `gateway://workspaces` with the whole list. On first
  start the list is empty until the local daemon answers `GET /v1/workspace`. An unreadable file
  is moved aside to `workspaces.json.invalid`.
- `keychain::TokenStore` with `OsKeychain` (`keyring`: Keychain Services, Credential Manager, the
  Secret Service over zbus) and `MemoryStore`. Only tests use it so far; remote pairing will.

## Tests

`tests/` runs on Unix (the fake daemon is an axum server on a unix socket laid out like
`pitcrewd serve --listen private`'s state directory); the unit tests run everywhere.

| File | What |
|---|---|
| `gateway.rs` | Requests whatever their status; the 1 MiB and 32 MiB limits; bad paths and methods refused before anything is sent; the error mapping (unknown workspace, daemon down, 503 and 404 on upgrade, `needs_pairing`); socket order with `close` last and the sink released; 1006; 1009 both ways; 1013 with exactly 8 MiB delivered; a webview that keeps up gets all 16 MiB; Pings; closing; cleanup per page. |
| `app.rs` | The commands through Tauri's IPC on the mock runtime with the real ACL: another window and other core commands are denied; `gateway://workspaces` on changes; closing the window closes its sockets. |
| `no_token.rs` | Every command, channel message, event, error and log line (at trace, Tauri's and tungstenite's records included) is searched for a known token; the fake daemon even echoes it in response headers. |
| `supervisor.rs` | Fake `pitcrewd` scripts: start and SIGTERM on quit; a growing backoff; giving up; never ready; no `pitcrewd`; a running daemon used, its workspace registered, and never stopped; starting our own when that one goes away. |

The OS keychain test is `#[ignore]`d: it needs an unlocked keychain (`cargo test -- --ignored`).
