# console (stream M)

The Agent console. See `docs/build/streams/M.md`. Import from `index.ts`: every component there
is a lazy chunk (render it inside a `<Suspense>`), so importing the console costs about 1 kB.

| File | What |
|---|---|
| `session-list.tsx` | `SessionList` (live from the hub) and `SessionListView` (from data it is given): virtualised, grouped by project → workstream plus *Unsorted*, a "Starting…" row, listbox keyboard navigation, `onSelect`. |
| `session-filters.tsx`, `facets.ts` | `SessionFilters`: machine, engine, state, project and workstream facets with counts. Controlled; pass the value to `SessionList`. |
| `chat-view.tsx`, `chat-rows.tsx` | `ChatView`: one session's transcript, newest page first, virtualised and anchored to its end. Older pages load on scroll-up (or when the view is not full) without moving the rows in view. |
| `question-card.tsx` | `QuestionCard`: an ask (answered through `POST /v1/asks/{id}/answer`), or a live transcript question with no ask (an option is picked with arrow keys and Enter; free text is sent as a prompt). |
| `composer.tsx` | `Composer`: Enter sends, Shift+Enter adds a line; Esc, Ctrl+C, Stop; disabled with the reason when the session has ended or cannot be reached (also after a 503). |
| `session-header.tsx` | `SessionHeader`: title, state, engine, agent, machine, branch, folder, task and workstream links, actions (End; Hand off, Fork and Review are disabled until given handlers). |
| `data.ts` | Hooks on `useLiveQuery` (see `src/data/README.md`): sessions with facets, machines, the transcript window, pending prompts, and the send, keys, interrupt, end and answer mutations. |
| `transcript.ts` | `TranscriptWindow` (merges pages by record offset; reports gaps) and `buildRows` (tool calls paired by `call_id`, questions folded with the call that asked them). No React. |
| `render/` | Markdown and diff parsing in a web worker (`worker.ts`, `client.ts`); the parser loads on the main thread only where no worker runs. `markdown.tsx` renders the parsed tree as elements: no HTML string anywhere, links only for http, https and mailto (`links.tsx`), opening outside the app; images are never loaded. |
| `api.ts`, `types.ts` | API calls and wire types `src/data` does not have yet (transcript, send, keys, interrupt, end, answer ask; `TranscriptItem`, `Key`, `EndMode`). They belong in `src/data`. |

## Transcripts

The newest page is `keys.sessions.transcript(id)`, the only one the stream refetches; older pages
are `keys.sessions.transcriptPage(id, before)` and never refetch. `TranscriptWindow` keeps every
record it has seen and which byte ranges are complete, so a newer tail never drops what the user
is reading, and a gap (more than a page arrived between two fetches) is fetched on its own. The
stream has no event for a prompt landing in a transcript, so `useTranscript` also refetches the
tail when the session's state or last activity changes.

## Links

Links open outside the app. The desktop shell passes its opener with
`<OpenExternalProvider open={…}>`; without it, links are plain `target="_blank"
rel="noopener noreferrer"` anchors.

## Tests

`src/console/tests`, run with `corepack pnpm --filter @pitcrew/ui exec vitest run --dir src/console`
(the package's `test` script covers `tests/` only). Component tests use happy-dom and the real mock
hub as a child process on a free port; `stubLayout()` gives virtual lists a size.
