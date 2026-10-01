# Brief M · Console components

- **Stream:** M · UI: Agent console. **Branch:** `s/M/console-components`. **Paths:**
  `apps/ui/src/console/**` only (plus `pnpm-lock.yaml` if you add a dependency, which is
  unlikely: ask in your report instead).
- **First read:** [README.md](README.md), then `docs/build/streams/M.md`, ADR-0008, ADR-0010,
  `docs/build/contracts/api-v1.md` (sessions, transcript paging, send/keys/interrupt, asks),
  `crates/protocol/src/transcript.rs`, and the merged UI on `main`: **`apps/ui/src/data/README.md`
  is mandatory** (use `useLiveQuery`, never plain `useQuery`), plus `src/design`.

## Goal

The building blocks of the Agent console, as tested components that work against the mock hub:
- the session list;
- the chat view, rendering transcripts tail-first;
- the composer;
- question cards.

The shell's feature registration arrives in brief L-shell. Until then, **build components, not
routes**. The next M brief wires them into the shell.

## What to build (all under `src/console`)

1. **Data hooks** (`src/console/data.ts`), built on the data layer's `useLiveQuery` and keys:
   - sessions (list, with filters) and one session;
   - the transcript as a tail-first infinite page list (`before=`), where only the newest page
     is live-invalidated;
   - mutations: send, keys, interrupt, end, and answering asks.

   Add any invalidation entries you need through the documented extension point. If none
   exists, propose one in your report rather than editing `src/data`.
2. **`SessionList`:**
   - virtualised (TanStack Virtual);
   - grouped by project → workstream, plus *Unsorted*;
   - each row shows the engine, title, the agent's handle, a state pill (working, waiting, idle,
     ended, unreachable), the live status line, and last activity;
   - a "Starting…" row for a session in `starting`;
   - keyboard navigation and a selection callback;
   - facet filters as a separate `SessionFilters` component (machine, engine, state, project,
     workstream).
3. **`ChatView`** for one session:
   - renders `TranscriptItem`s: prompts, assistant markdown, tool rows paired by `call_id`
     (collapsed, with target and summary, expandable to input and output), file edits with a
     diff viewer, plan updates as a checklist, questions, turn ends;
   - **markdown with no raw HTML** (sanitised; links get `rel="noopener noreferrer"` and open
     externally);
   - long transcripts load older pages on scroll-up without jumping;
   - heavy work (markdown, diff highlighting) runs in a **web worker** or at least lazily.
4. **`QuestionCard`:** shows a `Question` item or an open ask with options; answering sends the
   right API call. It is also usable in the Inbox later (stream N).
5. **`Composer`:**
   - multi-line; Enter sends, Shift+Enter adds a newline;
   - Esc and Ctrl+C buttons send keys; interrupt;
   - disabled with a reason when the session is ended or its machine is unreachable (503).
6. **`SessionHeader`:** title, engine, machine, branch, links to the task and workstream, and an
   actions menu (end, and placeholders for hand off, fork and review).
7. **Export** these from `src/console/index.ts` as named components, keeping the `feature` stub
   L's shell brief adds, if present.

## Acceptance

- Vitest and Testing Library, against the real mock hub (`startServer` on port 0, as L's tests
  do):
  - the list renders the demo sessions grouped correctly and updates live when a session's
    state changes;
  - `ChatView` renders every `TranscriptItem` kind from SES0001 and pages older items on
    scroll-up;
  - answering a `QuestionCard` calls the API and the ask closes;
  - sending from the composer appends the canned reply.
- **XSS test:** a transcript item containing `<img src=x onerror=…>`, `<script>`,
  `javascript:` links and raw HTML renders as text or inert markup.
- **Performance:** rendering 10,000 list rows stays virtualised (DOM row count bounded); the
  chat view with 5,000 items paints the newest page first.
- `typecheck`, `lint` (including the `useQuery` rule) and `build` pass; the initial bundle is
  not bloated (console code lazy-loaded).

## Out of scope

The terminal (xterm) view, the workbench tabs and splits, the file viewer, routing and
navigation (next briefs).
