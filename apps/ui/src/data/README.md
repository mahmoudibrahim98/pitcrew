# data (stream L)

The API client, the `/v1/stream` connection, and how events keep the TanStack Query cache fresh.
See `docs/build/streams/L.md` and `docs/build/contracts/api-v1.md`. Import from `src/data/index.ts`.

| File | What |
|---|---|
| `api.ts` | `createApi()`: fetch with `Authorization: Bearer`; every failure is an `ApiError { code, status }` (status 0 when unreachable, `internal` for a 2xx without JSON). Reads for every list in the contract, one `events` page, transcript pages; writes for projects and workstreams (create), tasks (create, patch, move, assign, subtasks, comment, dispatch), asks (answer), briefs (edit, pin, accept) and sessions (send, keys, interrupt, end). |
| `config.ts` | `VITE_PITCREW_API` (default `http://127.0.0.1:47317`) and `VITE_PITCREW_TOKEN` (default `dev-device-token`), dev server only; any build fails while the token is set. |
| `types.ts` | Hand-written wire types mirroring the serde names, until generated types exist: the model, `Brief`, `EventsPage` (alias `ActivityPage`), transcripts (`TranscriptItem`, `TranscriptPage`, `PlanItem`), `Key`, `EndMode`, and request bodies. `EVENT_TYPES` lists every `EventBody` type, `TRANSCRIPT_KINDS` every transcript item kind. |
| `keys.ts` | Query keys. Lists and details sit under separate prefixes (`['tasks', 'list', filters]`, `['tasks', 'detail', id]`). |
| `stream.ts` | `StreamClient`: subprotocol auth, resume by `since`, reset when `since` is ahead of `hello.rev`, when `hello.log` changes, or on a revision gap; capped back-off that starts over only after a stable connection; reconnect after 60 s of silence. No React. |
| `patches.ts` | Events that carry the whole object (`task_created`, `subtasks_replaced`, `session_discovered`) are written into the cache, except into queries already invalidated (they refetch anyway). |
| `invalidation.ts` | One entry per event type → the keys it touches. Every event also touches `['events']`. |
| `live.ts` | Wires it together: patches, then coalesced invalidation (250 ms window). Fetches in flight are not cancelled; each query whose fetch was in flight when an event touched it is refetched once, by its exact key, after that fetch settles. `resetQueries()` on a reset; failed queries refetch when the stream comes back. After 3 connection failures, and every 5 more, it probes `GET /v1/me` and reports `problem: 'unauthorized' \| 'unreachable'` (none when the probe succeeds and only the stream fails). |
| `provider.tsx` | `<DataProvider>`, `useApi()`, `useConnection()` (`status`, `synced`, `problem`), `useLiveQuery()`, `createQueryClient()`. |
| `hooks.ts` | Shared hooks: workspace, machines, members, projects, workstreams, tasks, sessions, asks, and `useMoveTask`. |

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
