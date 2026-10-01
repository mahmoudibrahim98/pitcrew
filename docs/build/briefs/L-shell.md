# Brief L · The shell

- **Stream:** L · UI foundation. **Branch:** `s/L/shell`. **Paths:** as in
  [L-skeleton-and-data.md](L-skeleton-and-data.md).
- **First read:** [README.md](README.md), then `docs/build/streams/L.md` (work packages 2, 4
  and 5), ADR-0008, the merged `apps/ui` on `main` (especially `src/data/README.md`), and the
  product's two layouts. The hybrid prototype's structure is described in ADR-0008; its look is
  the tokens in `packages/tokens`.

## Goal

The frame every feature lives in: the sidebar, **two layouts with a switcher**, workspace-scoped
routes, the command palette, the Orchestrator panel frame, and a **feature registration
interface**. With it, the console (M), projects (N) and onboarding (O) plug in without touching
the shell.

## What to build

1. **Feature registration** (`src/shell/features.ts` or similar). A feature exports from its
   folder's `index.ts` an object with:
   - its routes (TanStack Router subtree);
   - its nav entries (sidebar section, icon, label, badge source);
   - its palette commands;
   - which layout it belongs to (`projects` | `console` | `both`).

   The shell imports the three feature folders' `index.ts` and composes them. Ship **stub
   `index.ts` files** for `console`, `projects` and `onboarding`, each exporting an empty
   feature, so the app builds before those streams land. Those three files are the one exception
   to your paths; say so in your report. Document the interface in `src/shell/README.md`.
2. **Routes:** `/w/:ws/...`, with the workspace from `GET /v1/workspace` for now, a not-found
   page, and the proof page kept as a dev-only route.
3. **Sidebar:**
   - a workspace switcher (one workspace for now; built for many);
   - Home, Inbox (open-ask count badge), My tasks;
   - a Projects section listing projects, each expandable to its workstreams (from the data
     layer), with live updates;
   - an Agent console entry with a live count of working and waiting sessions;
   - collapsible, keyboard navigable.
4. **Two layouts and the switcher:** **Projects** and **Agent console**, switched with a toggle
   at the top left and **Ctrl .** (Cmd . on macOS). The choice persists per workspace.
5. **Palette (Ctrl K):**
   - fuzzy search over projects, workstreams, tasks (by key and title) and sessions, from the
     query cache, plus features' commands;
   - keyboard only, virtualised results.
6. **Orchestrator panel frame (Ctrl J):**
   - a resizable right-hand panel with an empty state;
   - its content comes later;
   - the open/closed state persists.
7. **"+ New"** menu: task, agent, project, team. Items open placeholder dialogs for now.
8. **Design components the shell needs** (`src/design`), on Radix:
   - menu, dialog, tooltip, kbd hint, avatar (person vs agent, agent shows its owner), badge,
     resizable panel, tree item;
   - all keyboard accessible, light and dark.

## Acceptance

- Playwright against the mock hub:
  - navigate Home → a project → a workstream;
  - switch layouts with Ctrl .;
  - open the palette with Ctrl K and jump to `PAP-4`;
  - open the Orchestrator panel with Ctrl J;
  - a task moved through the API updates the sidebar counts without a reload.
- axe reports no violations on the shell in either layout, light and dark.
- A stub feature registering a route and a nav entry appears in the sidebar and routes correctly
  (unit test).
- Initial JS is still under 250 KB gzipped; feature routes are code-split.

## Out of scope

Feature pages themselves (M, N, O). The Orchestrator panel's content.
