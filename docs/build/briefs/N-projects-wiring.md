# Brief N · Projects wiring: routes, pages, and the shared data layer

- **Stream:** N · UI: Projects layout. **Branch:** `s/N/projects-wiring`. **Paths:**
  `apps/ui/src/projects/**`.
- **First read:** [README.md](README.md), then [N-projects-components.md](N-projects-components.md)
  (merged), `docs/build/streams/N.md` (work packages 1–7), `apps/ui/src/shell/README.md`
  (feature registration, routes, commands, navigation), and `apps/ui/src/data/README.md` plus
  `api.ts`, `types.ts` and `hooks.ts` (stream L added your calls and types there).

## Goal

Make the Projects layout a working part of the app. A person can open Home, the Inbox, My tasks,
the projects list, a project, a workstream and a task from the shell, with live updates, against
the mock hub.

## What to build

1. **Feature registration** in `src/projects/index.ts` (keep the lazy component exports):
   - **Routes** under the workspace:
     - Home;
     - the Inbox;
     - My tasks;
     - the projects list;
     - a project (Overview, Workstreams, Board, Activity tabs);
     - a workstream (Where it stands, Board, Tasks, Agents, Activity);
     - a task, as a drawer over the page it was opened from, with its own URL.
   - **Nav entries and commands:** the sidebar's Inbox badge (`useInbox`); palette commands to
     open a project, workstream or task by key, and "New task".
   - Implement `ProjectsNavProvider` with the shell's navigation, so links in the components
     work.
   - Follow the shell README exactly. If the feature API lacks something, stop and describe it;
     don't edit `src/shell`.
2. **The shared data layer:** switch to `src/data`'s calls and types, and delete the local
   copies in `data.ts`. Swap your local `QuestionCard` for stream M's
   (`src/console/index.ts` exports it).
3. **Activity filters:**
   - The contract says the real hub answers `400 invalid` to `GET /v1/events?project=` and
     `?workstream=` until its index lands. Stream E is building that index.
   - Project and workstream activity must handle a 400 gracefully: show a short "not available
     yet" note, not an error, and keep task and session activity working.
4. **Pending proposals:** the contract change `integrator/work-edits` (approved, merging soon)
   adds `Brief.proposal`, the pending proposal, to `GET /v1/briefs`.
   - Once it's on `main`, switch "Where it stands" to read it instead of scanning events.
   - "Keep current" becomes a `PUT` of the current text.
   - Until then, keep the event scan.
5. **Pages the components don't cover yet** (N.md 3 and 7):
   - My tasks, as the board or list filtered to the person's tasks;
   - the projects list;
   - Members (people and agents in one table, owners shown).
   - Calendar can wait.

## Acceptance

- In the running app against the mock hub (Playwright, your own ports 47470–47479,
  `PLAYWRIGHT_CHANNEL=msedge`):
  - open Home from the sidebar;
  - open the Inbox and answer an ask;
  - open a project, then a workstream, then a task drawer, and check the URL reproduces it on
    reload;
  - move a task on the board;
  - follow a session link into the console;
  - the palette opens a task by key.
- axe passes on Home, Inbox, the board and the drawer in both themes.
- No projects code loads before a projects route is visited.
- Typecheck, lint, the tests, the build and `size` pass.

## Out of scope

Calendar, editing tasks through `PATCH` (it comes after work-edits merges), and the GitHub and
Jira panels.
