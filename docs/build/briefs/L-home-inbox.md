# Brief L · Home, Inbox and My tasks at the gr8r level

- **Stream:** L · Shell (Home, Inbox, My tasks).
  **Branch:** `integrator/home-inbox`.
  **Paths:** `apps/ui/src/shell/**` (Home, Inbox, My tasks pages), `apps/ui/src/projects/**` (only
  shared task rows), `apps/ui/src/design/**`, `apps/ui/src/data/**`, `crates/hub-work/**` and
  `crates/api/**` (only for read endpoints the views need), `crates/protocol/**`,
  `apps/mock-hub/**`, `tests/conformance/**`, `docs/build/contracts/api-v1.md`, and the READMEs of
  what you touch.
- **First read:**
  - [README.md](README.md) and the root `AGENTS.md` (or `CLAUDE.md`);
  - the gr8r Studio reference (`https://gr8r-studio.vercel.app`): its Home, Inbox and My Tasks;
  - `docs/adr/0008-two-layouts.md`;
  - the design tokens in `apps/ui/src/index.css` and `apps/ui/src/design/`.
- **Suggested agent:** an Opus-class agent with a strong design sense. Run it after
  0-tasks-that-work, whose drawer and toast it reuses.

## Goal

People compare PitCrew with the gr8r reference and "barely see anything of that". Bring Home,
Inbox and My tasks to that level, with agents as first-class.

## What to build

1. **Home.**
   - A greeting with the date.
   - Header actions: New task, New session, Invite.
   - Stat cards: active projects (at risk), open tasks (assigned to you), done this week, overdue,
     agents working now.
   - My tasks with Upcoming / Overdue / Completed tabs.
   - Agents now: working and waiting first, with engine logos and live state.
   - Project progress: status, progress bar, due date, team and agent avatars.
   - Upcoming deadlines grouped Today / Tomorrow / This week.
   - Recent activity, concise and grouped.
   - Each block has a designed empty state with the action that fills it.
2. **Inbox.**
   - Tabs: All, Needs you (asks and approvals), Assignments, Mentions, Updates.
   - An Unread filter, "mark all read", and two-pane reading: the item on the right, with the
     answer or reply right there.
   - Assigning a task to someone puts it in their Inbox.
3. **My tasks:** the list grouped by due date from 0-tasks-that-work, given the gr8r row design
   (priority, project dot, due colour, avatars) and keyboard navigation.
4. **Polish:**
   - engine logos and avatar colours;
   - consistent icons;
   - hover and focus states;
   - both themes;
   - compact and comfortable density.
5. **Tests:**
   - the views against the real hub and the mock;
   - axe in both themes;
   - visual snapshots where the repo's e2e supports them.

## Acceptance

- Side by side with the reference, Home, Inbox and My tasks carry the same information density and
  finish, plus PitCrew's agent layer.
- `npm test`, the UI's checks (typecheck, lint, unit, e2e), both conformance targets if the API
  changes, and the guards pass. Every CI job passes on the pull request.
