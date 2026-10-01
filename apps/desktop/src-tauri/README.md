# pitcrew-desktop

The PitCrew desktop app (ADR-0003): a Tauri 2 shell around the UI in `apps/ui`, the **gateway**
that is the webview's only way to a workspace's daemon, the supervisor of the person's local
`pitcrewd`, and the parts that work in the background: the tray, "needs you" notifications and
`pitcrew://` deep links. All logic stays in `pitcrewd`.

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

> **A debug build trusts whatever answers on `127.0.0.1:5173`.** It loads the dev server with no
> CSP (Tauri applies the CSP only to the bundled UI) and with devtools, and that page gets the
> gateway, and so the person's daemon. Run debug builds only on a machine where nobody else can
> listen on that port, with the Vite server started first (it must refuse another port:
> `strictPort`). Release builds never load it.

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

**Preferences.** `preferences.json` beside it is the app's own file, changed from the tray menu
(the app never writes `settings.json`):

```json
{ "notifications": true, "quitOnClose": false, "trayHintShown": true }
```

## In the background (`src/shell.rs`, `src/attention`, `src/notify`, `src/tray.rs`)

- **"Needs you"** (`attention/`). For each workspace that has been `ready`, the gateway keeps its
  own stream from the daemon, with the token it already holds (never the webview's): `/v1/stream`,
  then a snapshot (`GET /v1/me`, `GET /v1/asks?to=<me>&state=open`, `GET /v1/members`), then
  `ask_raised`, `ask_answered` and `member_added`. Only open asks addressed to the person count, as
  in their Inbox. A broken stream (a close, a broken connection, 60 s of silence) is resumed with
  `since` after 1 s, doubling to 30 s, or at once when the workspace becomes `ready` again; a new
  event log (`hello.log` changed, or `since` past the daemon's revision) takes a new snapshot.
  The wait starts again from 1 s only after a snapshot that worked or a stream that stayed up
  60 s, so a daemon that fails right after its `hello` is retried less and less often.
  **Bounds:** one watcher per workspace; 512 asks and 2048 names (60 characters each) per
  workspace (past 512 the count reads "512+", and an answer for an ask not kept asks for a
  snapshot, at most one a minute); 8 MiB per frame (a larger one starts afresh from a snapshot),
  16 MiB per snapshot body (a larger one reads "512+" and is tried again after 10 minutes), 30 s
  per request.
- **Notifications** (`notify/`), for new asks while notifications are on and the window is not in
  front (focused, visible and not minimised; asked once per stream frame):
  - title: the asker's name and the kind ("Writer has a question", "… needs a decision", "… asks for
    a review", "… asks for approval", "… mentioned you"); body: the ask's title and context. Both
    lose control and bidi characters and those that show as nothing (zero-width, the soft hyphen,
    tag characters, variation selectors, Hangul fillers and the like), have their whitespace
    collapsed, and are cut (the body at 200 characters); a body left empty reads "An agent needs
    you".
  - **Rate limit:** asks are gathered for 2 s, and notifications are at least 30 s apart; what
    arrives meanwhile becomes one ("3 agents need you", "Writer needs you" with "4 new asks").
  - **A click** navigates: to the ask's task, or the workspace's Inbox (a summary goes to the
    Inbox). Linux: `org.freedesktop.Notifications` on the session bus, with a `default` action
    (the title and body are escaped when the service reads markup); Windows: WinRT toasts
    (`tauri-winrt-notification`; a release build sends as the app's AppUserModelID, which the
    installer's shortcut must carry, a debug build as PowerShell's); macOS: the notification centre
    (`mac-notification-sys`; at most 4 notifications wait for a click, later ones only bring the
    app forward). No notification service (as in WSLg): logged once, nothing shown.
  - Nothing is asked of the webview: no notification permission is in its capability.
- **The tray** (`tray.rs`): "Open PitCrew"; one line per workspace ("Demo Lab — 3 need you"), which
  opens its Inbox; "Notify me when an agent needs me" and "Quit when the window closes" as check
  items; "Quit PitCrew" (the daemon stops only if the app started it). Rebuilt at most every
  250 ms. On Windows a left click opens the window.
  - **Is there a tray?** Always on Windows and macOS. On Linux only when a StatusNotifierWatcher
    with a host is on the session bus and libappindicator loads (it is loaded at run time; if it is
    missing, its loader panics, which the app catches): otherwise no icon. There the icon is
    handed over as a PNG in the app's cache directory (`tray/`), not the shared `/tmp/tray-icon`.
- **Closing the window** hides it when there is a tray and "Quit when the window closes" is off;
  the first time, a notification says so. Without a tray, closing quits, as before.

## Navigation from outside the window (`src/navigate.rs`, `src/scheme.rs`)

The [contract's](../../../docs/build/contracts/desktop-gateway.md) `gateway://navigate`: the app
emits a `NavigateTarget` (`{ workspace, kind, id? }`) to the main window, then shows and focuses
it.

- **Deep links** `pitcrew://w/<ws>/inbox` and `pitcrew://w/<ws>/<task|session|project|workstream>/<id>`.
  The raw text is parsed, never a URL library's normalised form: after the scheme (matched in any
  case) only `A-Z a-z 0-9 / _ -` may appear, so `..`, `%`-encodings, queries, fragments, user
  info and ports are refused rather than resolved; the shape must be exact (no empty segment,
  no trailing `/`); the workspace and ids must round-trip as ULIDs (bare or with their own
  prefix, any case), or a task's key (`PAP-4`) as written; at most 512 bytes. Anything else is
  dropped and logged, shortened and redacted. A link only navigates.
- **Where links come from:** the launch command line (Linux, Windows), the single-instance
  hand-over from a second launch, and macOS's `Opened` event. Clicks on the app's notifications
  use the same path.
  - **On macOS** Tauri hands links over already parsed, and the URL parser has resolved dot
    segments (`..`, `%2e`) by then: the strict parser sees the result, so such a link can still
    only open one of the allowed places.
  - **On Windows** the single-instance plugin joins a second launch's arguments with `|` and
    splits them again, so a link holding `|` arrives as several arguments, each parsed on its own.
- **A link that launched the app** arrives before the UI listens. The latest one is held until
  the main page calls `gateway_workspaces` (the UI listens to the gateway's events before it
  reads the list), for 60 s at most, and held again while the page reloads. The navigator exists
  before any plugin, so a link handed over while the app starts is held too.
- **Registering the scheme:** installers do it from `plugins.deep-link.desktop.schemes` in
  `tauri.conf.json`, which Tauri's bundler reads (the deep-link plugin itself is not used: it
  would emit every link, unparsed, to the webview). On Linux two kinds of run register
  themselves, as `xdg-mime default` would:
  - **an AppImage**, trusted only when `APPIMAGE` and `APPDIR` are set and the program runs from
    inside `APPDIR` (both variables are inherited by everything an AppImage starts);
  - **a debug build**, only with `PITCREW_DEV_REGISTER_SCHEME=1`:

    ```bash
    PITCREW_DEV_REGISTER_SCHEME=1 cargo run --manifest-path apps/desktop/src-tauri/Cargo.toml
    ```

  They write a hidden `$XDG_DATA_HOME/applications/org.pitcrew.desktop-url-handler.desktop`, and
  make it the default for `x-scheme-handler/pitcrew` in `$XDG_CONFIG_HOME/mimeapps.list` only
  when there is no default, when it is this handler already, or when the default names a handler
  that is gone (no `.desktop` file in the data directories, or its program missing): another
  installed PitCrew or another app is never replaced. Only that key changes and the file keeps
  its mode; a `mimeapps.list` that is a link (home-manager, stow, chezmoi) or over 1 MiB is left
  alone. A debug build on Windows is not registered (an installer registers it).

## The local daemon (`src/daemon`)

- **Finding it:** the configured path, then `pitcrewd` next to the app's executable, then (debug
  builds only) `PATH`, absolute entries only. A configured path that is not a program is an error.
- **Not a planted binary:** on Unix the program, its directory, and the directory of the path as
  given must be owned by root or us and not writable by group or others. On Windows a program
  downloaded from the web (with a `Zone.Identifier` stream) is refused; its owner is not checked
  yet (that needs the Win32 security API, which needs `unsafe`).
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
  never got ready) give up: the workspace is `unreachable` until the app restarts. A run of a
  minute or more starts the count and the wait afresh.
- **What the daemon says stays in the log.** A workspace's `detail` is the app's own sentence and
  the exit status. The daemon's stderr and ready line are logged only, in lines of at most 1 KiB,
  with anything token-shaped (`pcd_…`, `pca_…`, `pitcrew.bearer.…`, `Bearer …`, 40+ base64url
  characters) removed.
- **Someone else's daemon** is checked every 5 s (or at once when a request fails); if it goes
  away, the app starts its own.
- **Quitting:** the app stops the daemon only if it started it, also while it is still starting:
  SIGTERM and up to 8 s on Unix, then a kill. Windows has no signal one process can send another
  without a shared console, so there the daemon is terminated (SQLite's WAL keeps the store
  consistent).

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
  client messages, 8 MiB incoming frames per message. Time: 120 s per request, 20 s to open a
  socket (connect and upgrade), 10 s per frame sent to the daemon; past those, `unreachable` or a
  close with 1006.
- The gateway sends `Host`, `Content-Type` and `Authorization` (marked sensitive); the WebSocket
  carries `Sec-WebSocket-Protocol: pitcrew.v1, pitcrew.bearer.<token>`. Only the status, the
  `Content-Type` and the body come back.
- Text and close messages go on the channel as JSON; binary frames as raw IPC bodies
  (`ArrayBuffer`). `close` is always last, also after the webview's own close; then the channel is
  dropped, which ends it on the webview's side.
- **Back-pressure.** Each frame counts against an 8 MiB budget until the webview has taken it.
  Tauri's channels give no acknowledgement, so the gateway sends a probe (`eval_with_callback`
  of a no-op) after the frames; the webview runs those scripts in order. What a returned probe
  proves depends on the frame's size:
  - a small frame (JSON under 8 KiB, binary under 1 KiB) is delivered by its own script, so it
    has reached the page's handler;
  - a large frame's script only starts Tauri's fetch for it, so its bytes are released from the
    budget **when that fetch starts, not when the page has handled the frame**.

  One probe is in flight at a time. Past the budget the socket closes with 1013. An exact count
  needs the UI to acknowledge frames, a later contract change.
- **Pings** are answered by tungstenite as it reads; neither Ping nor Pong reaches the webview.
- **Cleanup.** A socket belongs to the webview that opened it. When that page starts loading again
  (a reload) or its window is destroyed, its sockets close with 1001 and nothing more is sent to
  it. A socket that finishes opening after its page reloaded is closed at once.

## The window, the CSP and the capability (`src/app.rs`, `tauri.conf.json`, `capabilities/`)

- One window, `main`, created in code with its guards: navigation away from the app's origin
  (`tauri://localhost`, `http://tauri.localhost` on Windows, or the dev server in debug builds) is
  refused, new windows and downloads are denied, devtools exist only in debug builds. The
  single-instance plugin focuses it on a second launch (and hands over its deep link).
- **The capability** (`capabilities/main.json`) gives `main` the five gateway commands and
  `core:event:allow-listen`/`allow-unlisten`, nothing else: no `core:default`, no shell, fs, http,
  opener or notification plugin, no emitting events. `build.rs` declares the commands in the
  app's ACL manifest, so no command runs without a capability naming it. The default `dynamic-acl`
  feature is off, so none can be added at run time. Tauri 2.12 cannot scope `listen` to one event
  name; the app emits only `gateway://workspaces` and `gateway://navigate`, and the webview
  cannot emit either. The tray, the notifications and the preferences add no command.
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
| `gateway.rs` | Requests whatever their status; the 1 MiB and 32 MiB limits; bad paths and methods refused before anything is sent; the error mapping (unknown workspace, daemon down, 503 and 404 on upgrade, `needs_pairing`, an upgrade that never answers); socket order with `close` last and the sink released; 1006; 1009 both ways; 1013 with exactly 8 MiB delivered; a webview that keeps up gets all 16 MiB; Pings; closing; cleanup per page. |
| `app.rs` | The commands through Tauri's IPC on the mock runtime with the real ACL: another window and other core commands are denied; `gateway://workspaces` on changes; closing the window closes its sockets; a deep link held until the main page asks for the workspaces, then `gateway://navigate` with the contract's payload, hostile links dropped, held again on a reload. |
| `attention.rs` | "Needs you" against the fake daemon's live stream: the snapshot (only the person's open asks), raised and answered, other members' asks and other events ignored, resuming with `since` without a snapshot, the bound and the snapshot it asks for, a new event log, one watcher per ready workspace, a reconnect at once when the workspace is ready again, a snapshot that keeps failing retried 50, 100, 200, then 400 ms apart (the cap) and from the first wait again once it works, and too many asks read as "N+" with no hot retry. |
| `no_token.rs` | Every command, channel message, event, error and log line (at trace, Tauri's and tungstenite's records included) is searched for a known token; the fake daemon even echoes it in response headers, and fake `pitcrewd`s print it on stderr and in the ready line; the "needs you" subscription (snapshot, live ask, reconnect), the notifications and tray lines it leads to, the navigation events, and a dropped deep link carrying the token are searched too; a canary record proves the log bridge works. |
| `supervisor.rs` | Fake `pitcrewd` scripts: start and SIGTERM on quit; quitting while it starts and while `token show-path` runs; a growing backoff; giving up; never ready; no `pitcrewd`; a running daemon used, its workspace registered, and never stopped; starting our own when that one goes away. |

Unit tests cover the rest: the deep-link parser as a table of hostile links, and a held target's
60 s (`navigate.rs`); the tracker (`attention/tracker.rs`); the text, the rate limiter and the
pacing, one look at the window per frame (`notify/mod.rs`); the D-Bus notifier against a fake
notification service over a private connection, clicks and closes included (`notify/linux.rs`);
the tray's lines; closing into the tray (`shell.rs`); the preferences; who registers the scheme,
the Linux handler and `mimeapps.list`: a foreign `APPIMAGE`, a debug build without the opt-in, a
foreign and a dangling default, a linked and an oversized file, the mode kept (`scheme.rs`).

The OS keychain test is `#[ignore]`d: it needs an unlocked keychain (`cargo test -- --ignored`).
