# Brief N · Projects and boards at the gr8r level

- **Stream:** N · Projects layout.
  **Branch:** `integrator/projects-boards`.
  **Paths:** `apps/ui/src/projects/**`, `apps/ui/src/design/**`, `apps/ui/src/shell/**` (sidebar
  project tree only), `apps/ui/src/data/**`, `crates/hub-work/**` (project settings and saved
  views if needed), `crates/protocol/**`, `apps/mock-hub/**`, `tests/conformance/**`,
  `docs/build/contracts/api-v1.md`, and the READMEs of what you touch.
- **First read:**
  - [README.md](README.md) and the root `AGENTS.md` (or `CLAUDE.md`);
  - the gr8r Studio reference: its Projects list, a project's Board, List, Table, Timeline and
    Files, and its task cards;
  - the current `apps/ui/src/projects/`.
- **Suggested agent:** an Opus-class agent with a strong design sense. Run it after
  0-tasks-that-work and 0-trusted-data.

## Goal

The audit found:

- the Projects page is a bare list;
- project pages have few views, and the Workstreams tab is a dead end with no "New workstream";
- there are no project settings;
- board cards show only an id and a title;
- six fixed-width columns overflow the window at 1200px;
- the Timeline is a single-week grid that says "Every task has a due date" when there are none.

## What to build

1. **Projects page.** A table with icon and colour, name, status, progress, due date, team and
   agents, sessions this week, and last activity, plus favourites, filters, sort and a New project
   button.
2. **Project header.**
   - Icon, favourite, a status dropdown, and lead and member avatars.
   - Settings: rename, key, folder, archive.
   - Views: Overview, Board, List, Table, Timeline, Calendar, Files, Activity, plus saved views
     (filters kept per person).
3. **Workstreams.** A "New workstream" button, inline status changes, and each workstream's
   folder, branch and machine shown in its header.
4. **Board.**
   - Cards with labels, priority, due date coloured by urgency, subtask progress, comment count,
     assignee or agent avatar, and the live agent state when an agent is working the task.
   - Each column has a count, an "add task" control and a menu.
   - Columns fit the window or scroll inside the board, never the page.
   - Drag and drop keeps working.
5. **Timeline.**
   - A real Gantt: bars from start to due date, a milestones row, dependency arrows, Weeks and
     Months views, grouping by workstream, and Today.
   - Correct copy when nothing has dates.
6. **Sidebar tree:** project icons and colours, favourites first, counts that mean something.
7. **Tests:**
   - views and interactions against the real hub and the mock;
   - axe in both themes;
   - the board at 1200 and 900px with no page overflow.

## Acceptance

- Side by side with the reference, the Projects page and a project's views carry the same
  information density and finish, with agents visible where they work.
- `npm test`, the UI's checks, both conformance targets if the API changes, and the guards pass.
  Every CI job passes on the pull request.
