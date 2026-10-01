# Brief L · Recaps in the data layer

- **Stream:** L · UI foundation. **Branch:** `s/L/recap-data`. **Paths:** `apps/ui/src/data/**`.
- **First read:** [README.md](README.md), `docs/build/contracts/api-v1.md` ("Recaps": the types, the
  UTF-8 byte spans, paging, and the **live-update rule** for the data layer),
  `apps/mock-hub/src/types.ts` (the recap types as the mock declares them) and
  `apps/mock-hub/README.md`, and `apps/ui/src/data/README.md`.

## Goal

Features (stream N first) can show recaps: blocks of work with their one-line summaries, and day
paragraphs, every clause linked to its receipts, kept current as events arrive.

## What to build

1. **Types** in `types.ts`, mirroring `crates/protocol/src/recap.rs`: `Block`, `BlockKey`,
   `Counts`, `FileTouch`, `Fact` and all its `FactKind` variants, `Check`, `Span`, `Summary`,
   `RecapBlock`, `BlocksPage`, `DayRecap` and `DaysPage`.
2. **Calls and keys:**
   - `api.recapBlocks(filters, before?, limit?)` and `api.recapDays(scope, tz, before?, limit?)`;
   - `keys.recaps.all`, `keys.recaps.blocks(filters)` and `keys.recaps.days(scope, tz)`.
3. **Hooks,** as infinite queries on `useLiveQuery`'s conventions:
   - `useRecapBlocks(filters)` pages by the last block's id;
   - `useRecapDays(scope)` pages by the last date, with `tz` from the viewer's current offset
     (`-new Date().getTimezoneOffset()`).
   - The mock serves only `tz=0`. Make the hooks take an override, so the e2e suites and the mock
     can pass `0`, and document it.
4. **Clauses:** `clauses(summary) → { text, receipts }[]`. The spans are **UTF-8 byte ranges**:
   convert through `TextEncoder`, and never slice the JavaScript string with them. Also return the
   joining text between spans, so a feature can render the whole paragraph with the clauses
   marked.
5. **Live updates:** implement the contract's invalidation rule exactly.
   - Events that change no recap are ignored.
   - `member_added` invalidates every recap key.
   - Any other event invalidates the unfiltered blocks key, plus every recap key whose filter
     value is in the event's scope. Resolve the scope through the cache, including old and new
     links for `session_linked` and `task_updated`.
   - When the cache can't resolve a link, invalidate every recap key.
   - Table-test every rule.

## Acceptance

- **Vitest against the mock hub:**
  - both routes through the hooks, with paging to `at_start`;
  - `clauses()` with multi-byte text: "É", "−", emoji, and a span that would be wrong if sliced
    as a JavaScript string;
  - every invalidation rule.
- Typecheck, lint, the tests, the build and `pnpm size` all pass. The initial JS shouldn't grow
  noticeably; recap code that only features use may stay in `src/data`, but say what it costs.

## Out of scope

The UI (stream N), read cursors, and the real routes (streams H and E).
