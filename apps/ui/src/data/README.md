# data (stream L)

The API client, the `/v1/stream` connection with resume, and the event → query invalidation map.
See `docs/build/streams/L.md` and `docs/build/contracts/api-v1.md`. Import from `src/data/index.ts`.

| File | What |
|---|---|
| `api.ts` | `createApi()`: fetch with `Authorization: Bearer`, errors as `ApiError { code, status }` (status 0 when unreachable). |
| `config.ts` | `VITE_PITCREW_API` (default `http://127.0.0.1:47317`) and `VITE_PITCREW_TOKEN` (default `dev-device-token` in dev only). |
| `types.ts` | Hand-written wire types mirroring the serde names, until generated types exist. `EVENT_TYPES` lists every `EventBody` type. |
| `keys.ts` | Query keys. Lists and details sit under separate prefixes (`['tasks', 'list', …]`, `['tasks', 'detail', id]`). |
| `invalidation.ts` | One entry per event type → the keys it touches. Every event also touches `['events']`. |
| `stream.ts` | `StreamClient`: subprotocol auth, resume by `since`, reset when `since` is ahead of `hello.rev`, back-off, reconnect after 60 s of silence. No React. |
| `live.ts` | Wires the stream to a `QueryClient`: invalidate on events, `resetQueries()` on a reset. |
| `provider.tsx` | `<DataProvider>`, `useApi()`, `useConnection()` (stream status), `createQueryClient()`. |
| `hooks.ts` | Shared query hooks: workspace, members, projects, workstreams, tasks, sessions, asks, and `useMoveTask`. |

## Rules for features

- Read through `useApi()` and key your queries with `keys` (or a new prefix added here), so the
  stream's invalidation reaches them. Queries never go stale on their own (`staleTime: Infinity`);
  the stream is what refreshes them.
- Mutations do not write the cache; the event that the change emits does. If a new event type
  appears in the contract, add it to `types.ts` and `invalidationMap` (a test fails until you do).
