# Desktop gateway

How the UI, running in the desktop app's webview, reaches each workspace's daemon.

- **The webview never holds a token** (ADR-0003). It never opens a socket or makes an HTTP request
  to a daemon itself. It calls the gateway, the Rust side of the desktop app (stream K). The
  gateway adds the workspace's device token and forwards the call over the daemon's socket, pipe or
  SSH tunnel.
- The calls below are Tauri commands. Stream L's data layer implements the UI side
  (`apps/ui/src/data`); stream K implements the gateway (`apps/desktop`).
- Requests and messages are exactly API v1 ([api-v1.md](api-v1.md)). The gateway does not
  interpret them, apart from the checks below.
- **Changes** go through a contract change, like the API's. A new optional field or command is not
  breaking.

## Conventions

- **Arguments:** Tauri passes arguments by their names. `gateway_request` takes one argument,
  `req`: the UI calls `invoke('gateway_request', { req: {...} })`. The socket commands take flat
  arguments, as written below.
- **Field names** are camelCase on the wire, as written here (`contentType`). On the Rust side,
  use `#[serde(rename_all = "camelCase")]`.
- **Versions:** the `tauri` crate and `@tauri-apps/api` share major.minor (2.12 today); the Tauri
  CLI refuses a mismatch.
- **`connecting`:** a workspace that is `connecting` may answer `unreachable` until it is
  `ready`. The UI retries, and reconnects at once on `ready`.

## Knowing where the UI runs

The UI is in the desktop app when `window.__TAURI_INTERNALS__` exists. There it uses the gateway
for everything, and never reads `VITE_PITCREW_API` or a token. In a browser it uses HTTP and
WebSockets as today (development only).

## Workspaces

`gateway_workspaces() → GatewayWorkspace[]`

```ts
interface GatewayWorkspace {
  id: string;        // the daemon's workspace id (a ULID), as in `/w/$ws/…`
  name: string;
  kind: 'local' | 'remote';
  state: 'connecting' | 'ready' | 'unreachable' | 'needs_pairing';
  detail?: string;   // why it is unreachable or needs pairing, for people to read
}
```

The gateway emits the Tauri event `gateway://workspaces` with the same list whenever it changes.
The UI's `/w/$ws` routes use these ids. Adding, pairing and removing workspaces are later
commands (onboarding, stream O).

## Requests

`gateway_request(req: GatewayRequest) → GatewayResponse`

```ts
interface GatewayRequest {
  workspace: string;
  method: 'GET' | 'POST' | 'PATCH' | 'PUT' | 'DELETE';
  path: string;      // '/v1/…', with an optional '?query'
  body?: string;     // JSON text
}
interface GatewayResponse {
  status: number;
  contentType?: string;
  body: string;      // the response body as text (API v1 bodies are JSON); "" when there is none
}
```

- **Every daemon answer is a response,** whatever its status: a 404 or 409 comes back as
  `{status, body}` with the `ApiError` body, as over HTTP.
- **The gateway checks:**
  - `path` starts with `/v1/`;
  - it has no `..` segment, no `//`, no `\`, no `#` and no control characters;
  - its query, if any, is passed on as it is;
  - `body` is at most 1 MiB, and is sent with `Content-Type: application/json`.
- **The webview sets no headers.** The gateway sends `Authorization`, `Content-Type` and `Host`
  itself.
- **The response** carries only `status`, `contentType` and the body. No other daemon header
  reaches the webview. The gateway refuses a response body over 32 MiB.
- **Gateway failures** reject the command's promise with a `GatewayError`. A request the daemon
  never answered is one of these, never a made-up HTTP status:

```ts
interface GatewayError {
  code: 'unknown_workspace' | 'needs_pairing' | 'unreachable' | 'invalid' | 'too_large' | 'internal';
  message: string;
}
```

## Sockets

The live stream (`/v1/stream`) and terminals (`/v1/sessions/{id}/terminal`) go through gateway
sockets, which carry the same frames as API v1's WebSockets.

`gateway_socket_open({ workspace, path, events: Channel }) → { socket: number }`

- **Paths:** only `/v1/stream` and `/v1/sessions/{id}/terminal`, with their queries. Any other
  path is `invalid`.
- **Opening:** the gateway opens the WebSocket to the daemon with
  `Sec-WebSocket-Protocol: pitcrew.v1, pitcrew.bearer.<token>`. The command resolves once the
  upgrade has succeeded.
- **When the upgrade fails,** it rejects with a `GatewayError`. The daemon's HTTP status, if
  there was one, is in `message`, and `unavailable` maps to `unreachable`.
- **Messages on the `events` channel,** in order:
  - `{ "type": "text", "data": string }` is a text frame;
  - an `ArrayBuffer` is a binary frame (a raw IPC body, not JSON);
  - `{ "type": "close", "code": number, "reason": string }` is always the last message, also
    after the webview's own `gateway_socket_close`. When the connection to the daemon breaks
    without a close frame, the code is 1006.
- **The gateway answers the daemon's Pings itself.** The webview never sees Ping or Pong.

`gateway_socket_send({ socket, text?: string, binary?: number[] | Uint8Array })`

- Exactly one of `text` and `binary`. The size limits are API v1's: 1 MiB on terminals, 4 KiB
  on the stream. Over a limit, the gateway closes the socket with 1009, as the daemon would.
- Sending on a closed socket rejects with `invalid`.
- **Order.** Tauri may run commands concurrently, so the UI keeps at most one
  `gateway_socket_send` in flight per socket. The command resolves once the gateway has queued the
  frame to the daemon, in that order.

`gateway_socket_close({ socket, code?: number, reason?: string })` closes the socket (1000 by
default). It is idempotent.

**Back-pressure.** The gateway keeps at most 8 MiB of incoming frames that the webview has not
taken. Past that, it closes the socket with 1013, and the UI reconnects with `since` or `from` as
it already does.

**Cleanup.** When the webview reloads or the window closes, the gateway closes every socket that
window opened.

## Navigation from outside the window

Deep links (`pitcrew://w/<ws>/inbox`, `pitcrew://w/<ws>/task/<id>`,
`pitcrew://w/<ws>/session/<id>`, `pitcrew://w/<ws>/project/<id>` and
`pitcrew://w/<ws>/workstream/<id>`), and clicks on the app's own notifications, open a place in the
UI. The gateway emits the Tauri event `gateway://navigate`, then shows and focuses the main window.
Its payload is a typed target, not a path:

```ts
interface NavigateTarget {
  workspace: string;                                              // a workspace id (ULID)
  kind: 'inbox' | 'task' | 'session' | 'project' | 'workstream';
  id?: string;                                                    // required unless kind is 'inbox'
}
```

- **The gateway accepts only those link shapes.** Every id is a bare ULID or a known display
  form. Anything else from a deep link is dropped and logged (shortened), never forwarded.
- **The UI checks the target again,** maps it to its own route (`paths.task(ws, id)`, and so on),
  and navigates. An unknown workspace goes to `/` with a notice.
- **Deep links arrive from any web page,** so they never act: they only navigate. No deep link
  answers an ask, moves a task, or sends input.

## Security notes

- Only the app's own windows can call these commands (Tauri capabilities). No remote URL is ever
  loaded into a window.
- The gateway logs workspace ids, paths without their query, statuses and timings. It never logs
  tokens, bodies or frames.
- A test in the gateway proves that no token reaches the webview: no command's result, event or
  error ever contains it.
