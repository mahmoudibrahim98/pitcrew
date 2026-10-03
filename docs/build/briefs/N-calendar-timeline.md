# Brief N · Calendar and Timeline views

- **Stream:** N · UI: Projects layout. **Branch:** `s/N/calendar-timeline`.
  **Paths:** `apps/ui/src/projects/**`.
- **First read:**
  - [README.md](README.md) and the root `AGENTS.md` (or `CLAUDE.md`);
  - `docs/build/streams/N.md` (items 4 and 7);
  - `apps/ui/README.md` and `apps/ui/src/projects/README.md`;
  - `apps/ui/src/shell/README.md` (feature registration);
  - `docs/build/contracts/api-v1.md` (tasks, workstreams, `due`).
- **Suggested agent:** Codex, or any coding agent.

## Goal

Stream N's card plans a **Calendar** and a project **Timeline**; neither exists. Build both from data
the API already serves. Tasks have an optional `due` date (`CalendarDate`, no time zone).

## What to build

1. **Calendar** (a page in the projects layout, registered from `projects/index.ts` like Home and My
   tasks):
   - a month grid of tasks by `due` date, filterable to "mine", a project, or a workstream;
   - previous, next and today controls; a task opens the existing task drawer;
   - keyboard: arrows move between days, Enter opens a day's tasks, and focus stays visible;
   - **at phone width,** the grid becomes a list of days with tasks;
   - an empty state that says how to give a task a due date.
2. **Timeline** (a tab on the project page):
   - one row per workstream, with its tasks placed by `due` date on a shared time axis
     (weeks or months, with a zoom toggle);
   - tasks without a due date are listed apart, not dropped;
   - a "today" line; status shown by form as well as colour (an outline for open, filled for done,
     and so on), so it doesn't rely on colour alone;
   - it scrolls horizontally inside its own container, never the page.
3. **Dates:**
   - use the existing `CalendarDate` helpers (`projects/format.ts`), and read dates as dates, not
     instants (no time-zone shifts);
   - weeks start per the browser's locale where `Intl` exposes it.
4. **Live updates:** both views update when a task's `due` or status changes through the stream,
   as the board does today.

## Tests

- **Unit tests:** placing tasks on the grid and on the axis (month edges, leap day, no due date), and
  keyboard navigation.
- **An end-to-end test** against the mock hub, with an axe check in both themes.
  - If the demo data has too few due dates to show the views, use the UI tests' own fixtures.
    Don't change `crates/fixtures`, and say so in the report.

## Acceptance

- `npm run typecheck`, `npm run lint`, `npm test`, `npm run build` and the end-to-end tests pass in
  `apps/ui`.
- The guards pass, and every CI job passes on the pull request.

## Out of scope

Editing due dates by dragging, which needs a contract change (propose it in the report), and
calendars outside projects.
