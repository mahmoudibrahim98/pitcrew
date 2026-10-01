# Brief N · Recaps in the Projects layout

- **Stream:** N · Projects layout. **Branch:** `s/N/recap-views`. **Paths:** `apps/ui/src/projects/**`.
- **First read:** [README.md](README.md), `docs/build/streams/N.md`, `docs/build/contracts/api-v1.md`
  ("Recaps"), `apps/ui/src/data/README.md` (`useRecapBlocks`, `useRecapDays`, `clauses()`, and the
  `tz` override), `apps/ui/src/projects/README.md` (`activity.tsx`, `receipts.tsx`, the
  workstream and project pages), and `apps/mock-hub/README.md` (the mock serves only `tz=0`).

## Goal

People see what happened, in prose they can check: on a workstream or project, a paragraph per day,
and every clause is a link to the evidence behind it. This is one of PitCrew's distinguishing
features, so it has to be trustworthy and pleasant to read.

## What to build

1. **The Activity "Summary" tab** (today a placeholder in `activity.tsx`), for a project and for a
   workstream:
   - day paragraphs, newest first (`useRecapDays`), grouped by date; on a project, by workstream
     within a date, with the work outside any workstream first;
   - **every clause is marked and focusable.** Hovering or focusing it shows its receipts with the
     existing receipts UI, and activating it opens them (events, sessions, tasks, files);
   - joining text is plain;
   - below each paragraph, a disclosure lists the day's blocks with their one-line summaries
     (`useRecapBlocks` filtered to the scope, matched by id);
   - "Load older days" pages to `at_start`;
   - empty, loading and error states as the rest of the layout has them.
2. **Session and task pages:** show the blocks of work for that session or task, with their lines
   and counts (files touched, tools run and failed, turns), newest first, with "Load older".
3. **Text safety:** recap text is untrusted. Render it as text, never as HTML or markdown. Clause
   ranges are UTF-8 bytes, so always go through `clauses()`.
4. **Time zones:** use the viewer's offset. The e2e suite and component tests pass `tz: 0` through
   the hooks' override, because the mock serves only `tz=0`.
5. **Accessibility:**
   - a clause with receipts is a button (or a link, if it navigates) with an accessible name that
     says it has evidence;
   - the receipts popover is reachable and dismissable by keyboard;
   - axe passes.

## Acceptance

- **Vitest against the mock hub:**
  - the Summary tab for PRJ0001 and for one of its workstreams;
  - clause marking with multi-byte text (from the fixture, or a synthetic summary through a fake
    API);
  - receipts open from a clause;
  - paging to the start;
  - session and task blocks.
- **Playwright** (own ports, `PLAYWRIGHT_CHANNEL=msedge`):
  - open the project's Activity, then Summary, and see the demo's paragraphs;
  - activate a clause by keyboard and see its receipts;
  - open a workstream's Summary;
  - axe in both themes and both layouts.
- Typecheck, lint, the tests, the build and `pnpm size` all pass. Keep the recap views in the lazy
  Projects chunk.

## Out of scope

"Since you last looked" (read cursors, later), Home's recap, editing briefs, and the data layer
itself (stream L).
