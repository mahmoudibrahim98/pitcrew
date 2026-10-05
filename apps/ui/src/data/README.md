# data (stream L)

Task archival is `patchTask(id, { archived: true | false })`, carried by `task_updated`.
Task lists retain archived documents for resolving dependencies; work views filter them.
`dispatch_finished` refreshes dispatches and sessions, and `session_ended` refreshes dispatches
too, so task run status and failure summaries follow the recorded end without polling.

The API client, the `/v1/stream` connection, and how events keep the TanStack Query cache fresh.
See `docs/build/streams/L.md`, `docs/build/contracts/api-v1.md` and
`docs/build/contracts/desktop-gateway.md`. Import from `src/data/index.ts`.

`cursors.ts` exposes live queries and mutations for the person's read cursors. The
`cursor_moved` stream event invalidates `['cursors']`, including writes on another
device; it is excluded from recap invalidation. Each workspace's existing data scope
keeps its cursor cache separate, and failed mutations retain the previous cursor.

| File | What |
|---|---|
| `transport.ts` | The seam every request and socket goes through: `Transport` (`request(method, path, body) → { status, contentType, body }`, `openSocket(path) → TransportSocket`, and, in the desktop app, `storeCredential(integration, secret)`: an integration's secret through the gateway's own command, never `request`). `browserTransport()`: `fetch` and `WebSocket` with the bearer token (development). `TransportSocket.bufferedAmount` is bytes sent but not yet taken: the real `WebSocket`'s own, here. `isDesktop()`: `window.__TAURI_INTERNALS__` exists. |
| `gateway.ts` | The desktop transport: the gateway's commands and channels, exactly as the contract says. No token, header or `VITE_*` value. `GatewaySocket.bufferedAmount` is the bytes still queued in its own outbox; past 8 MiB there the socket closes itself with 1013 (`the sender ignored back-pressure`), as the daemon does to a receiver that falls behind — a sender is expected to watch it, the same as a browser `WebSocket`'s. |
| `desktop.tsx` | The desktop app's workspaces: `gateway_workspaces()` and `gateway://workspaces`, and one data scope per workspace (`Workspaces`, `<WorkspacesProvider>`). `open(id)`/`leave(id)` track which workspace is in view: a workspace left in the background for `backgroundMs` (10 minutes) has its stream closed, resumed with `since` by the next `open()` — see "Workspaces in the desktop app" below. `onNavigate()` subscribes to `gateway://navigate` before the first `gateway_workspaces()` call — the gateway holds a launch-time deep link until then — and holds a target that still arrives first until the list is known (`pendingNavigateMs`, 60 s, after which it is dropped rather than acted on with a stale list), so every delivery carries the list to check the target against. With `gateway.ts` and `@tauri-apps/api`, loaded only in the desktop app, by dynamic import; elsewhere import from them with `import type` only. |
| `workspaces.tsx` | What the shell sees of them: `useGatewayWorkspaces()` (`null` in a browser), `<WorkspaceScope ws>`, `useGatewayNavigate(onTarget)` (follows `gateway://navigate`, raw and unvalidated — the shell checks it, `shell/gateway-navigate.ts` — together with the workspace list, already known by the time it fires), `useRemoteGateway()` and `useGatewayPrompts()` (both `null` in a browser: see "Remote workspaces and prompts" below), and the gateway's types. |
| `remote.ts` | Remote workspaces and SSH's prompts: the contract's types (`RemoteProbe`, `RemotePlanRequest`, `RemotePlan`, `RemoteProgress`, `GatewayPrompt`, `PromptReply`), the `RemoteGateway` interface, and the checks every answer and event passes (`parseRemoteProbe`, `parseRemotePlan`, `parseProgress`, `parsePrompt`, …). No Tauri: `gateway.ts` implements it. |
| `setup.ts` | The first run: `setUp(api, queryClient, setup)`, `useSetUp()` and the `useSetup()` mutation (`POST /v1/setup`). It refreshes the workspace, `me`, members and machines itself (see "The first run" below); a `409` is a `SetupConflict` saying whether the workspace was `alreadySetUp`. |
| `root.tsx` | `<AppData>`: picks the data layer once, at start, and loads it on demand: `desktop.tsx` in the desktop app; `browser.tsx` in a browser, in development only. A production build outside the desktop app fails closed: an error screen, no request. |
| `browser.tsx` | The browser's data layer (development): `<DataProvider>` over `browserTransport` with `config.ts`. Never loaded in the desktop app; not in production builds. |
| `errors.ts` | `ApiError { code, status }`, and `GatewayError` (an `ApiError` with status 0 and the gateway's own `gateway` code). |
| `api.ts` | `createApi({ transport })` (or `{ baseUrl, token, fetch }`, which makes a browser transport): every failure is an `ApiError` (status 0 when unreachable, `internal` for a 2xx without JSON). Reads for every list in the contract, one `events` page, transcript pages; writes for projects and workstreams (create), tasks (create, patch, move, assign, subtasks, comment, dispatch), asks (answer), briefs (edit, pin, accept) and sessions (send, keys, interrupt, end). |
| `config.ts` | `browserConfig()`: `VITE_PITCREW_API` (default `http://127.0.0.1:47317`) and `VITE_PITCREW_TOKEN` (default `dev-device-token`), dev server only; any build fails while the token is set. Loaded only by `browser.tsx`. The only file that may read `import.meta.env` (ESLint; `import.meta.env.DEV` is allowed anywhere). |
| `types.ts` | Hand-written wire types mirroring the serde names, until generated types exist: the model, `Brief`, `EventsPage` (alias `ActivityPage`), transcripts (`TranscriptItem`, `TranscriptPage`, `PlanItem`), recaps (`Block`, `Summary`, `Span`, `BlocksPage`, `DaysPage`, …), `Key`, `EndMode`, and request bodies. `EVENT_TYPES` lists every `EventBody` type, `TRANSCRIPT_KINDS` every transcript item kind, `CHECKS` every `Check`. |
| `keys.ts` | Query keys. Lists and details sit under separate prefixes (`['tasks', 'list', filters]`, `['tasks', 'detail', id]`); `keys.recaps.blocks(filters)` and `keys.recaps.days(scope, tz)` hold every page an infinite query has loaded (`keys.recaps.all` is the prefix for "every recap key"). |
| `stream.ts` | `StreamClient({ transport })`: resume by `since`, reset when `since` is ahead of `hello.rev`, or when `hello.log` changes; accepts private revision gaps; capped back-off that starts over only after a stable connection; reconnect after 60 s of silence, or at once with `retryNow()`. `streamPath()`, `terminalPath()`. No React. |
| `patches.ts` | Events that carry the whole object (`task_created`, `subtasks_replaced`, `session_discovered`) are written into the cache, except into queries already invalidated (they refetch anyway). |
| `invalidation.ts` | One entry per event type → the keys it touches. Every event also touches `['events']`. Recaps have their own rule (`recaps.ts`'s `recapScopeMap`, wired in by `live.ts`), since it needs the query cache, not just the event. |
| `recaps.ts` | `clauses(summary)`: a `Summary`'s text split into its receipted clauses and the plain text joining them, converting the contract's UTF-8 byte spans correctly. `useRecapBlocks(filters, { limit? })` and `useRecapDays(scope, { tz?, limit? })`: infinite queries (`useLiveInfiniteQuery`) paged backwards by the last block's id / day's date; `.blocks`/`.days` flatten the loaded pages, `.atStart`, `.loadMore()`. `tz` defaults to the viewer's own offset (`-new Date().getTimezoneOffset()`); pass `0` against the mock hub and from e2e suites, which only have days for `tz=0`. Also `recapScopeMap`/`recapScopeForEvents`: the event → scope half of the live-update rule (API v1, "Recaps"), cache-free and table-tested; `live.ts` supplies the cache and turns the scope into query keys. |
| `live.ts` | Wires it together: patches, then coalesced invalidation (250 ms window). Fetches in flight are not cancelled; each query whose fetch was in flight when an event touched it is refetched once, by its exact key, after that fetch settles. `resetQueries()` on a reset; failed queries refetch when the stream comes back. After 3 connection failures, and every 5 more, it reports `problem: 'unauthorized' \| 'unreachable' \| 'needs_pairing'`: from the gateway's reason when the socket gave one, otherwise by probing `GET /v1/me` (none when the probe succeeds and only the stream fails). `recapCacheLookup()`/`recapKeysForScope()` resolve recap scope through the cache into query keys: `Invalidator.addExact()` invalidates a key by itself only, never as a prefix, for the unfiltered-blocks case (TanStack matches `{}` against any object, so the usual prefix invalidation would reach every filtered blocks query too). |
| `provider.tsx` | `<DataProvider>` (a browser's one hub), `<DataScope>`, `useApi()`, `useConnection()` (`status`, `synced`, `problem`), `useLiveQuery()`, `useLiveInfiniteQuery()`, `useOpenSocket()`, `useGatewayWorkspace()`, `createQueryClient()`. |
| `hooks.ts` | Shared hooks: workspace, machines, members, projects, workstreams, tasks, sessions, asks, and `useMoveTask`. |

## Transports: the browser and the desktop app

`<AppData>` (in `main.tsx`) picks one transport at start and nothing else changes for features:
they call `useApi()`, `useLiveQuery()` and `useOpenSocket()` either way.

- **Browser** (development only): `fetch` and `WebSocket` to `VITE_PITCREW_API`, with the dev
  token as `Authorization: Bearer` and as the `pitcrew.bearer.` subprotocol. A production build
  opened in a browser shows an error screen and reaches no daemon.
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

A workspace's stream keeps running in the background once opened, so switching back shows a fresh
cache at once — for up to 10 minutes out of view (`Workspaces.leave()`, called when its
`WorkspaceScope` unmounts): past that its stream closes, to cap how many stay fully connected while
nobody is looking. Switching back to it (`open()`, cancelling the pending close if it was still
waiting) starts its stream again, resuming with `since` from the stream's own last revision, same
as any other reconnect; its cache is untouched meanwhile, so nothing refetches that a `since`
catch-up will not also cover. When the gateway marks a workspace `ready` again, its stream
reconnects at once (also when a connection attempt was still out). A workspace that is
`unreachable` or `needs_pairing` shows that state in the frame, not a spinner.

The gateway emits `gateway://workspaces` only on changes, so a failed `gateway_workspaces()` is
read again with back-off (1 s doubling to 30 s) until the list is known; the shell shows the error
with a Retry button (`useGatewayWorkspaces().retry()`).

## The first run: `setup_needed` and `useSetup()`

`GET /v1/workspace` answers `setup_needed: true` while the hub has no person (api-v1.md, "The
first run"); `useWorkspace().data.setup_needed` carries it, and the shell sends such a workspace to
the first-run wizard. `POST /v1/setup` is the one write that updates the cache itself, because
nothing in the event log says the workspace was named or set up:

- on success, the workspace's entry gets the new name and `setup_needed: false`, and `me` the new
  person, **at once**, so the shell never sees the stale flag and sends the person back into the
  wizard; then the workspace, `me`, members and machines are invalidated (refetched, which also
  cancels any fetch from before setup still in flight);
- on `409`, it reads `GET /v1/workspace` again into the cache, and rejects with a `SetupConflict`
  whose `alreadySetUp` says which `409` it was: the workspace is set up (go Home) or the handle is
  taken;
- anything else (`400 invalid`, say) rejects with the `ApiError` unchanged.

`useSetup()` works in any data scope: the browser's hub, or one desktop workspace (a remote one
just added, say), through that workspace's gateway transport.

## Remote workspaces and prompts (desktop only)

`Gateway.remote` (`remote.ts`, implemented in `gateway.ts`) carries the gateway's remote commands,
exactly as desktop-gateway.md says: `sshHosts()`, `remoteProbe(host)` (with `tmux?.version` when
the gateway gives it), `remotePlan(req)` (sent as one argument, `req`, like `gateway_request`),
`remoteAdd(plan, onProgress)` (progress on a `Channel`: each `step` one of the plan's, the last
`{ step: 'add', … }` for the whole add), `workspaceRemove(workspace, stopHelper)`,
`workspaceRetry(workspace)`, `remoteCancel(plan)` (a gateway without the command rejects),
`onPrompt`, `onPromptClosed` and `replyPrompt(id, { answer } | { accept } | {})`. Prompt kinds are
`password`, `passphrase` and `otp` (an `answer`), `host_key` and `confirm` (`accept`), and
`notice` (nothing to answer); `kind` says who asks, and the UI never guesses it from `text`.

- **Every payload is checked.** A malformed answer to a command rejects with a `GatewayError`
  (`internal`); a malformed progress message, prompt or `prompt-closed` is dropped (with one
  warning that never quotes it). Workspaces, from the list, its event or `remoteAdd`, pass one
  check (`toGatewayWorkspace`), which keeps `detail` only as a string; a list payload that is not a
  list never replaces a known list (a read that is not one is retried, as a failed read is).
- **Text** for people loses its control characters, except new lines and tabs in a prompt's
  `text` and a progress `detail`. A long one keeps its **end**, where ssh's question (or the news)
  is, behind a `…`. Plan steps and progress steps are cleaned the same way (`cleanStep`), so a
  message still names its step. A job script is kept verbatim.
- **Prompts** queue in the registry (`Workspaces.prompts`, oldest first; the same `id` again, as
  the gateway sends after a page reload, keeps its place instead of queueing twice). The registry
  follows `prompt`, `prompt-closed`, `workspaces` and `navigate` together, **before** its first
  `gateway_workspaces` read: the gateway holds a prompt raised before the page listens (a
  reconnect at launch) until that read. `gateway://prompt-closed` withdraws one; `replyPrompt`
  takes one off and sends the reply once. **The queue never holds an answer**, and a refused reply
  is not logged (its message could quote what was sent): the answer is read from the dialog's
  uncontrolled field as it is sent (`shell/prompt-dialog.tsx`).
- **In a browser** there is no registry: `useRemoteGateway()` and `useGatewayPrompts()` are `null`,
  and the UI says that connecting a machine needs the desktop app.

## Sockets for features: `useOpenSocket()`

The console's terminal (stream M) opens its socket through the same transport as the stream:

```ts
import { terminalPath, useOpenSocket } from '../data/index.ts';

const open = useOpenSocket();
useEffect(() => {
  const socket = open(terminalPath(session, { cols, rows, from }));
  socket.onmessage = ({ data }) => (typeof data === 'string' ? control(data) : write(new Uint8Array(data as ArrayBuffer)));
  socket.onclose = (close) => reconnectUnless(close?.code === 1000); // 1011, 1013, 1006: reconnect with `from`
  socket.send(new TextEncoder().encode('ls\r'));                    // keystrokes: a binary frame
  socket.send(JSON.stringify({ type: 'resize', cols, rows }));      // control: a text frame
  return () => {
    // Detach first, as StreamClient.stop() does: the close we ask for is still reported to
    // `onclose`, which would otherwise reconnect after the component has gone.
    socket.onopen = socket.onmessage = socket.onerror = socket.onclose = null;
    socket.close();
  };
}, [open, session]);
```

`TransportSocket` behaves the same in both transports:
- `onopen`; `onmessage` (text as strings, binary as `ArrayBuffer`s); `onerror`;
  `onclose({ code, reason, error? })`, once and last, **also after your own `close()`**. Set the
  handlers to `null` before closing a socket you are done with.
- `send()` before it opens waits for it, in order; `close()` before it opens sends nothing.
- In the desktop app, a frame the gateway refuses ends the socket with 1011 (the frames after it
  are dropped, not sent with a hole before them), and a close the gateway cannot do ends it with
  1006. Either way, reconnect.

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

**Recaps** (`recaps.ts`): `useRecapBlocks(filters)` and `useRecapDays(scope)` keep every loaded
page under one key (`keys.recaps.blocks(filters)` / `keys.recaps.days(scope, tz)`), unlike
transcripts' separate tail/page keys — a `useLiveInfiniteQuery` refetches every one of its pages
together on invalidation, which is what keeps an older page in step with a change to an open
block's line. `tz` defaults to the viewer's own offset; **the mock hub, and anything run against
it (e2e suites included), must pass `{ tz: 0 }`** — it only has days for `tz=0` and answers
`400 invalid` for any other value. Rendering the clauses a `Summary` names: `clauses(summary)`
splits `summary.text` into its receipted clauses and the plain joining text, converting the UTF-8
byte spans correctly — never `summary.text.slice(span.range.start, span.range.end)`, which is
wrong as soon as the text holds a character outside ASCII.

`api.createPersona`/`editPersona` and `createTeam`/`editTeam` use the device-only directory write
routes. Their `persona_saved`, `member_added` and `team_saved` events already invalidate the shared
lists; creation forms also refresh their own lists on success. Machines expose their optional
reported `info.os` to validate creation roots against that machine rather than the browser OS.
