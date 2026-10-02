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
optional; `PITCREW_PITCREWD`, `PITCREW_STATE_DIR`, `PITCREW_SSH`, `PITCREW_ASKPASS` and
`PITCREW_HELPERS` override them:

```json
{ "pitcrewd": "/opt/pitcrew/bin/pitcrewd", "stateDir": "/home/sam/.local/share/pitcrew",
  "ssh": "/usr/bin/ssh", "askpass": "/opt/pitcrew/bin/pitcrew-askpass",
  "helpers": "/opt/pitcrew/helpers" }
```

Without `stateDir` the app uses the daemon's own default, so it finds a `pitcrewd` started by
hand. Without `ssh`, `askpass` and `helpers` it uses `ssh` on `PATH` and what it was installed
with (see "Remote workspaces"). Paths must be absolute. Logs go to stderr at
`PITCREW_DESKTOP_LOG` (default `info`).

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

## Remote workspaces (`src/remote`)

The contract's "Remote workspaces" and "Prompts", on stream J's `pitcrew-remote` and the
person's own OpenSSH and `~/.ssh/config`:

```js
invoke('gateway_ssh_hosts')                                    // → { hosts: string[] }
invoke('gateway_remote_probe', { host })                       // → RemoteProbe
invoke('gateway_remote_plan', { req: { host, launcher, site, job } })  // → { plan, steps, jobScript? }
invoke('gateway_remote_add', { plan, events: channel })        // → GatewayWorkspace
invoke('gateway_workspace_retry', { workspace })
invoke('gateway_workspace_remove', { workspace, stopHelper })
invoke('gateway_prompt_reply', { id, answer })                 // or { id, accept }; { id } cancels
// events: gateway://prompt (GatewayPrompt), gateway://prompt-closed ({ id })
```

- **Probe** runs `pitcrew-remote`'s probe, then asks the direct launcher (or, for a SLURM
  helper, the scheduler) what is installed and running: `helper: { version, running }`;
  `slurm` when `sbatch` is there; `tmux: { version }` when tmux is (the tmux launcher needs 3.2
  or newer).
- **Plan** changes nothing on the machine. It probes again, finds the helper for the machine's
  platform (below), checks the launcher (tmux 3.2 or newer; for SLURM the tools and the site
  recipe: the built-in `generic`, or `~/.pitcrew/sites/<name>.toml`), and for SLURM renders the
  job script from the person's options (`time` as SLURM writes it, never `UNLIMITED`; `gpus` a
  count or `type:count`, sent as `--gres=gpu:…`). A job of the helper's already queued or
  running is used instead: the plan says so and holds no script. The plan is kept under a
  random id for 10 minutes, at most 32 at once, and used once: `add` takes it whatever happens.
- **Add** carries the plan out, step by step, with progress on `events`: each step of the plan
  as `{ step, state: "running" }` then `"done"` or `"failed"` (with `detail`; the upload adds
  `"running"` messages with `"N% sent"`, and a SLURM job its state, e.g.
  `"job 4242 pending (Priority)"`), ending with one `{ step: "add", state: "done" | "failed" }`.
  1. *Deploy*: the helper's bytes are checked against their sha256 before anything is sent, and
     again on the machine (`pitcrew_remote::deploy`).
  2. *Launch*: the direct or tmux launcher starts it; for SLURM, exactly the plan's script is
     submitted, and the add waits for the job to run and its helper to listen (10 minutes,
     asking every 2 s), saying how it stands.
  3. *Connect*: the tunnel (`pitcrew_remote::Connector`) is started and must connect (3
     minutes).
  4. *Pair*: the hub's device token is read over SSH (`cat` of the file `pitcrewd token
     show-path` names, between this call's random begin and end markers, so a login shell's
     chatter around it does not count; at most 8 KiB) and checked by asking the hub
     `GET /v1/workspace` with it. **The hub's answer is not trusted**: its workspace id is
     claimed in the registry in one step that refuses an id already held by the local
     workspace, or by a remote one on another machine (another host or root), with "already
     added as <name>; remove it first"; only the same machine may be paired again (its entry is
     replaced). Only then is the token kept in the keychain under that id (if that fails, the
     claim is undone), and the workspace is `ready`. Its name is cleaned like a notification's
     text and cut to 80 characters. (A local daemon that reports a remote workspace's id is
     refused the same way: the local workspace is `unreachable`, saying so.)

  If a step fails after this add started the helper (or submitted its job), it is stopped (or
  cancelled) again, within 90 s; if that fails too, the error (and the add's last `detail`) says
  so: "PitCrew's helper may still be running on <host>", or "job <id> may still be queued on
  <host>; cancel it with scancel <id>". Errors: a refused plan or launch is `invalid`, a lost
  connection, a cancelled prompt or a job that does not start in time `unreachable`.
- **Retry** (`gateway_workspace_retry`) starts a remote workspace's tunnel over now
  (`Connector::retry()`), for instance after a sign-in was cancelled while reconnecting, which
  leaves it `unreachable` until then; a tunnel that could not be made at start is made again.
  It returns once the attempt has started; the state follows on `gateway://workspaces`.
- **Remove** forgets a remote workspace and deletes its keychain entry, after stopping its
  helper (`stopHelper: true`; for SLURM, cancelling its job). If the stop fails, nothing is
  forgotten. The local workspace cannot be removed.
- **Connections.** The registry keeps, for each remote workspace, its host, launcher, the
  helper's root and platform, for SLURM the site, the job options and the last hop, and the
  transport the tunnel found worth remembering (`Connector::transport()`), never a secret. Each
  has a tunnel and a task following it: `ready` while connected (`needs_pairing` instead when its
  token is no longer in the keychain); `connecting` while connecting or while the way does not
  answer (the reason in `detail`); `unreachable` with the reason once the tunnel gave up for now
  (it keeps trying as `pitcrew-remote` says, or at a retry). Requests and sockets go through
  `Connector::connect()` with the token from the keychain, read for each connection. At start
  the tunnels of saved workspaces are made again; at quit they close.
- **Waking up.** Tauri tells the app nothing about sleep or network changes on the desktop, so a
  5 s timer that fires 10 s or more late means the computer slept, and every tunnel is told to
  check at once (`wake()`). The tunnel notices a jumped wall clock itself; this catches Windows,
  whose monotonic clock runs during sleep. Network changes are not watched.
- **Prompts** from every remote ssh call (the tunnel's reconnections included) go through
  `pitcrew-askpass` to `gateway://prompt` `{ id, host, kind, text, fingerprint? }` in the main
  window, and wait for `gateway_prompt_reply`. Kinds: `password`, `passphrase` and `otp` take
  `answer`; `host_key` (with its `fingerprint`, the `SHA256:…` or `MD5:…` of the question) and
  `confirm` (ssh's other yes/no questions, such as "Accept updated hostkeys?") take `accept`; a
  `notice` ("touch your security key") takes no answer and is closed when ssh moves on. A reply
  with neither cancels (ssh stops; a `confirm` is answered no). A reply of the wrong shape, or
  an answer over 4 KiB, is `invalid` and the prompt stays open. `text` loses control, bidi and
  invisible characters (line breaks kept) and is cut at 2000 characters. The answer goes to ssh
  once, in a `Secret`, and is never logged or kept. Every prompt ends with
  `gateway://prompt-closed { id }`: answered, or stale (ssh stopped waiting, the call ended).
  Open prompts are emitted again the first time the main page asks for the workspaces after it
  (re)loads; the UI keys them by id.
- **What the app ships for it** (stream P):
  - `pitcrew-askpass` next to the app's executable (`pitcrew-askpass.exe` on Windows), or
    `askpass` in the settings. Without it no remote command runs ssh: they fail with a clear
    `internal` error. It is checked like `pitcrewd` (owner and mode on Unix).
  - The helpers, in `helpers/` in the app's resources or next to its executable, or `helpers`
    in the settings: `pitcrewd-x86_64-unknown-linux-musl`, `pitcrewd-aarch64-unknown-linux-musl`,
    `pitcrewd-universal-apple-darwin` (`Platform::artefact()`), each checked like `pitcrewd` on
    Unix. A missing helper is a clear `invalid` error at plan time.
  - **Their checksums, compiled in** (ADR-0009): build the app with `PITCREW_HELPERS_MANIFEST`
    set to `{ "version": "<pitcrewd --version's second word>", "sha256": { "<file>": "<hex>" } }`.
    A **release build** trusts only those: without them, planning fails with `invalid` ("this
    build has no helper checksums"), and a `manifest.json` beside the helpers is never read,
    also when the settings name another helpers folder (the compiled checksums still apply).
    Only a **debug build** without compiled checksums reads `manifest.json` there
    (development).
- **ssh** is `ssh` on `PATH`, as the person runs it; a configured `ssh` (settings or
  `PITCREW_SSH`) is checked like `pitcrewd` (owner and mode on Unix) and refused otherwise, with
  a clear `internal` error. It gets only `pitcrew-remote`'s minimal environment. The host must
  be one ssh takes as a name (no leading `-`); a site recipe's name is `a-z 0-9 _ -`. Messages
  and logged reasons pass through `redact` and lose control, bidi and invisible characters.

## The window, the CSP and the capability (`src/app.rs`, `tauri.conf.json`, `capabilities/`)

- One window, `main`, created in code with its guards: navigation away from the app's origin
  (`tauri://localhost`, `http://tauri.localhost` on Windows, or the dev server in debug builds) is
  refused, new windows and downloads are denied, devtools exist only in debug builds. The
  single-instance plugin focuses it on a second launch (and hands over its deep link).
- **The capability** (`capabilities/main.json`) gives `main` the gateway's twelve commands (the
  five of workspaces, requests and sockets, and the seven of remote workspaces and prompts) and
  `core:event:allow-listen`/`allow-unlisten`, nothing else: no `core:default`, no shell, fs, http,
  opener or notification plugin, no emitting events. `build.rs` declares the commands in the
  app's ACL manifest, so no command runs without a capability naming it. The default `dynamic-acl`
  feature is off, so none can be added at run time. Tauri 2.12 cannot scope `listen` to one event
  name; the app emits only `gateway://workspaces`, `gateway://navigate`, `gateway://prompt` and
  `gateway://prompt-closed`, to the main window, and the webview cannot emit any. The tray, the
  notifications and the preferences add no command.
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
  A remote workspace's connection is `{ "type": "remote", "host", "launcher", "root",
  "platform", "site"?, "job"?, "lastHop"?, "transport"? }`. States live in memory; every change
  emits `gateway://workspaces` with the whole list. On first start the list is empty until the
  local daemon answers `GET /v1/workspace`. An unreadable file is moved aside to
  `workspaces.json.invalid`.
- `keychain::TokenStore` with `OsKeychain` (`keyring`: Keychain Services, Credential Manager, the
  Secret Service over zbus) and `MemoryStore` (tests). Remote workspaces' tokens live there,
  under the workspace's id (service `org.pitcrew.desktop`).

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
| `remote.rs` | Remote workspaces through the IPC, against a fake machine (`remote/fake.rs`: this computer's `/bin/sh` in a temporary home behind a fake `ssh` that plays links, ControlMasters, forwards and sessions, and asks through askpass) whose helper is the **real `pitcrewd --demo`**. Direct: probe (with `tmux` when there is one), plan and add on a machine whose start-up files print banners and a false token marker, the workspace `ready`, `gateway_request` and a socket reaching the hub through the forwarded socket, the transport saved, the token the hub's own, the helper seen running, remove stopping it and deleting the keychain entry, a plan used once, and another window refused by Tauri's ACL for all seven remote commands (with a real prompt open and a real workspace, which stay so). SLURM (stand-in `sbatch`, `squeue`, `scancel`, `sinfo`): the plan's exact script submitted byte for byte though the site recipe changed after planning, a job that stays pending cancelled again, bad job options and sites refused. Prompts: a host key and a password answered in the app, an answer over 4 KiB and a reply of the wrong shape refused with the prompt still open, then a cancelled password failing the add cleanly (nothing deployed or registered, no empty password sent, every prompt withdrawn). Takeover: a hub reporting the id of a remote on another machine, or of the local workspace, refused with that workspace and its token untouched, the helper stopped again, and a stop that fails named in the error. A restart: the saved remote `ready` again, `needs_pairing` once its token is gone. A sign-in cancelled while reconnecting, then `gateway_workspace_retry` back to `ready`. An expired plan, a missing helper, a missing `pitcrew-askpass` and a refused configured `ssh`. No token in any result, channel message, event or log line, after an add and after a refused pairing. Its own `main` (`harness = false`): the binary is also the fake `ssh` and `pitcrew-askpass`. `pitcrewd` is `PITCREW_TEST_PITCREWD`, else built once from the root workspace into the target's temporary folder. |

Unit tests cover the rest: the deep-link parser as a table of hostile links, and a held target's
60 s (`navigate.rs`); the tracker (`attention/tracker.rs`); the text, the rate limiter and the
pacing, one look at the window per frame (`notify/mod.rs`); the D-Bus notifier against a fake
notification service over a private connection, clicks and closes included (`notify/linux.rs`);
the tray's lines; closing into the tray (`shell.rs`); the preferences; who registers the scheme,
the Linux handler and `mimeapps.list`: a foreign `APPIMAGE`, a debug build without the opt-in, a
foreign and a dangling default, a linked and an oversized file, the mode kept (`scheme.rs`); a
configured `ssh` others can write refused (`app.rs`); the prompt hub (a password round trip,
cancel, stale prompts withdrawn and replayed, host keys and their fingerprints, each kind's own
reply shape, the 4 KiB cap, text cleaning; `remote/prompt.rs`), plans used once, expired and
pushed out, site names and job options checked (`remote/plan.rs`), helpers by platform, their
hash and owner checks, a release build trusting only compiled checksums, and askpass
(`remote/helpers.rs`), the tunnel's states as workspace states, `needs_pairing` without a token
(`remote/link.rs`), the error codes and progress shape, the token read between its markers, a
hub's name cleaned and cut, a failed undo in the error (`remote/mod.rs`), and remote records
saved, reloaded, given their transport and removed, a remote refused another workspace's id and
the local daemon refused a remote's (`registry.rs`).

The OS keychain test is `#[ignore]`d: it needs an unlocked keychain (`cargo test -- --ignored`).
