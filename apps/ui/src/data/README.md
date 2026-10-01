# data (stream L)

The API client, the `/v1/stream` connection, and how events keep the TanStack Query cache fresh.
See `docs/build/streams/L.md`, `docs/build/contracts/api-v1.md` and
`docs/build/contracts/desktop-gateway.md`. Import from `src/data/index.ts`.

| File | What |
|---|---|
| `transport.ts` | The seam every request and socket goes through: `Transport` (`request(method, path, body) → { status, contentType, body }`, `openSocket(path) → TransportSocket`). `browserTransport()`: `fetch` and `WebSocket` with the bearer token (development). `isDesktop()`: `window.__TAURI_INTERNALS__` exists. |
| `gateway.ts` | The desktop transport: the gateway's commands and channels, exactly as the contract says. No token, header or `VITE_*` value. |
| `desktop.tsx` | The desktop app's workspaces: `gateway_workspaces()` and `gateway://workspaces`, and one data scope per workspace (`Workspaces`, `<WorkspacesProvider>`). With `gateway.ts` and `@tauri-apps/api`, loaded only in the desktop app, by dynamic import; elsewhere import from them with `import type` only. |
| `workspaces.tsx` | What the shell sees of them: `useGatewayWorkspaces()` (`null` in a browser), `<WorkspaceScope ws>`, and the gateway's types. |
| `root.tsx` | `<AppData>`: picks the transport once, at start. In a browser, `<DataProvider>` over `browserTransport` with `config.ts`; in the desktop app, it loads `desktop.tsx`. |
| `errors.ts` | `ApiError { code, status }`, and `GatewayError` (an `ApiError` with status 0 and the gateway's own `gateway` code). |
| `api.ts` | `createApi({ transport })` (or `{ baseUrl, token, fetch }`, which makes a browser transport): every failure is an `ApiError` (status 0 when unreachable, `internal` for a 2xx without JSON). Reads for every list in the contract, one `events` page, transcript pages; writes for projects and workstreams (create), tasks (create, patch, move, assign, subtasks, comment, dispatch), asks (answer), briefs (edit, pin, accept) and sessions (send, keys, interrupt, end). |
| `config.ts` | `browserConfig()`: `VITE_PITCREW_API` (default `http://127.0.0.1:47317`) and `VITE_PITCREW_TOKEN` (default `dev-device-token`), dev server only; any build fails while the token is set. Called only in a browser. |
| `types.ts` | Hand-written wire types mirroring the serde names, until generated types exist: the model, `Brief`, `EventsPage` (alias `ActivityPage`), transcripts (`TranscriptItem`, `TranscriptPage`, `PlanItem`), `Key`, `EndMode`, and request bodies. `EVENT_TYPES` lists every `EventBody` type, `TRANSCRIPT_KINDS` every transcript item kind. |
| `keys.ts` | Query keys. Lists and details sit under separate prefixes (`['tasks', 'list', filters]`, `['tasks', 'detail', id]`). |
| `stream.ts` | `StreamClient({ transport })`: resume by `since`, reset when `since` is ahead of `hello.rev`, when `hello.log` changes, or on a revision gap; capped back-off that starts over only after a stable connection; reconnect after 60 s of silence, or at once with `retryNow()`. `streamPath()`, `terminalPath()`. No React. |
| `patches.ts` | Events that carry the whole object (`task_created`, `subtasks_replaced`, `session_discovered`) are written into the cache, except into queries already invalidated (they refetch anyway). |
| `invalidation.ts` | One entry per event type → the keys it touches. Every event also touches `['events']`. |
| `live.ts` | Wires it together: patches, then coalesced invalidation (250 ms window). Fetches in flight are not cancelled; each query whose fetch was in flight when an event touched it is refetched once, by its exact key, after that fetch settles. `resetQueries()` on a reset; failed queries refetch when the stream comes back. After 3 connection failures, and every 5 more, it reports `problem: 'unauthorized' \| 'unreachable' \| 'needs_pairing'`: from the gateway's reason when the socket gave one, otherwise by probing `GET /v1/me` (none when the probe succeeds and only the stream fails). |
| `provider.tsx` | `<DataProvider>` (a browser's one hub), `<DataScope>`, `useApi()`, `useConnection()` (`status`, `synced`, `problem`), `useLiveQuery()`, `useOpenSocket()`, `useGatewayWorkspace()`, `createQueryClient()`. |
| `hooks.ts` | Shared hooks: workspace, machines, members, projects, workstreams, tasks, sessions, asks, and `useMoveTask`. |

## Transports: the browser and the desktop app

`<AppData>` (in `main.tsx`) picks one transport at start and nothing else changes for features:
they call `useApi()`, `useLiveQuery()` and `useOpenSocket()` either way.

- **Browser** (development): `fetch` and `WebSocket` to `VITE_PITCREW_API`, with the dev token as
  `Authorization: Bearer` and as the `pitcrew.bearer.` subprotocol.
- **Desktop app**: `gateway_request`, `gateway_socket_open`/`send`/`close` and
  `gateway_workspaces`, through `@tauri-apps/api` (loaded on demand; `pnpm size` fails if it
  reaches the initial JS). **No token, `Authorization` header or `VITE_PITCREW_*` value is read
  or sent there** (`tests/desktop-no-token.test.tsx`).
  - A `GatewayError` is an `ApiError` with status 0, so existing handling applies: `unreachable`
    is `code: 'unavailable'`, the same as a network failure; `needs_pairing` is
    `code: 'unauthorized'` with `gateway: 'needs_pairing'`, and the stream reports it as its own
    `problem`. `unknown_workspace` → `not_found`; `invalid` and `too_large` → `invalid`.
  - Binary channel messages arrive as `ArrayBuffer`; the `close` message ends the socket with its
    code (`onclose({ code, reason })`); a socket the gateway could not open ends with 1006 and the
    `GatewayError` as `error`.
  - Sends go one at a time, in order (the gateway's commands may run concurrently otherwise);
    consecutive binary frames are joined into one.

## Workspaces in the desktop app: one QueryClient each

The desktop app lists the gateway's workspaces (`useGatewayWorkspaces()`; `null` in a browser)
and follows `gateway://workspaces`. **Each workspace gets its own `QueryClient`**, API client and
stream, made the first time it is opened (`<WorkspaceScope ws>`, keyed by `ws`, in the shell's
frame). We chose this over putting the workspace in every key because:

- a cache can then never hold, or show, another workspace's data, whatever a feature's key is;
- `keys.ts`, `invalidation.ts`, `patches.ts` and every feature's hooks stay as they are: a key
  like `['tasks', 'list', filters]` is per workspace by construction, and an event from one
  workspace's stream can only touch that workspace's cache;
- TanStack Query observers bind to their client when they mount, so the scope is keyed by `ws`:
  switching remounts the frame and nothing of the old workspace lingers on screen.

A workspace's stream keeps running in the background once opened, so switching back shows a
fresh cache at once. When the gateway marks a workspace `ready` again, its stream reconnects at
once. A workspace that is `unreachable` or `needs_pairing` shows that state in the frame, not a
spinner.

## Sockets for features: `useOpenSocket()`

The console's terminal (stream M) opens its socket through the same transport as the stream:

```ts
import { terminalPath, useOpenSocket } from '../data/index.ts';

const open = useOpenSocket();
const socket = open(terminalPath(session, { cols, rows, from }));
socket.onmessage = ({ data }) => (typeof data === 'string' ? control(data) : write(new Uint8Array(data as ArrayBuffer)));
socket.onclose = (close) => reconnectUnless(close?.code === 1000); // 1013: reconnect with `from`
socket.send(new TextEncoder().encode('ls\r'));                  // keystrokes: a binary frame
socket.send(JSON.stringify({ type: 'resize', cols, rows }));    // control: a text frame
socket.close();
```

`TransportSocket` behaves the same in both transports: `onopen`, `onmessage` (text as strings,
binary as `ArrayBuffer`s), `onerror`, `onclose({ code, reason, error? })` once and last; `send()`
before it opens waits for it, in order; `close()` before it opens sends nothing.

## Rules for features

**Read with `useLiveQuery`, never plain `useQuery`.** The stream connects first; `useLiveQuery`
keeps a query idle until the stream's first `hello`, so every fetch reflects at least that
revision and every later change arrives as an event. A plain `useQuery` can fetch before that,
and an event landing between its fetch and the `hello` is lost for good (queries never go stale on
their own: `staleTime: Infinity`). ESLint rejects `useQuery` and friends outside `src/data`.

**Adding a hook** in your feature folder:

```ts
import { keys, useApi, useLiveQuery } from '../data/index.ts';

export function useWorkstreamSessions(workstream: string) {
  const api = useApi();
  return useLiveQuery({
    queryKey: keys.sessions.list({ workstream }),
    queryFn: ({ signal }) => api.sessions({ workstream }, signal),
  });
}
```

- Key it under an existing prefix in `keys.ts` when the data is the same kind (a filtered task or
  session list), so the stream's invalidations and patches already reach it.
- **List data under `['tasks', 'list', …]` and `['sessions', 'list', …]` must be a plain array of
  the wire objects**, with the filters object as the key's third element: `patches.ts` edits those
  arrays in place and matches items against the filters. Derive shapes with `select`, not by
  storing something else under these keys. (A non-array is detected and refetched instead, with a
  console warning.)
- **Invalidation belongs to stream L.** There is no run-time way for a feature to add entries: the
  map is one table so that every event's effect is visible, typed per event and tested in one
  place. If your data is a new kind, or an event should touch a key it does not yet touch, ask
  stream L for the key in `keys.ts` and the entry in `invalidation.ts` (say which events change it
  and which keys, as in "`session_state_changed` → the newest transcript page"). Until it lands,
  key the data under the nearest existing prefix, or note the gap in your report.
- A new API call goes in `api.ts`, with its types in `types.ts` (also stream L's; ask).

**Writes:** mutations do not touch the cache; the event the change emits does (`useMoveTask` is
the pattern). If a new event type appears in the contract, add it to `types.ts` and
`invalidationMap`; the tests fail until you do, including one that reads the Rust `EventBody`.

**Transcripts:** key the newest page `keys.sessions.transcript(id)` and older pages
`keys.sessions.transcriptPage(id, before)`. Live events refetch only the newest page:
`session_state_changed`, `turn_ended`, `tool_ran` and `file_edited` touch it.
