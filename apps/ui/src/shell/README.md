# shell (stream L)

The frame every feature lives in: the sidebar, the two layouts and their switcher, the
workspace-scoped routes, the palette, the Orchestrator panel frame, "+ New", and the **feature
registration interface** through which the console (M), projects (N) and onboarding (O) plug in.
See `docs/build/streams/L.md` and ADR-0008.

| File | What |
|---|---|
| `feature.ts` | The interface: `Feature`, `NavEntry`, `Command`, `CreateEntry`, `defineFeature`. |
| `index.ts` | What features import from the shell. |
| `routes.tsx` | `createAppRouter(features)`: `/`, `/w/$ws` (the frame), the features' routes and the placeholders. |
| `registry.ts` | Composes the shell's own feature with the registered ones; `servedPaths`, `withLayout`. |
| `core.tsx` | The shell's own feature: Home, Inbox, My tasks, Agent console, its commands, placeholder routes and "+ New" dialogs. |
| `layout.ts` | Which layout is on, switching (`switchLayout`), and remembering where each layout was left. |
| `store.ts` | UI state (zustand, persisted as `pitcrew.shell`). |
| `frame.tsx`, `sidebar.tsx`, `projects-tree.tsx`, `top-bar.tsx`, `orchestrator.tsx`, `create.tsx` | The frame's parts. |
| `palette.tsx`, `fuzzy.ts` | The palette (a lazy chunk) and its fuzzy matching. |
| `pages/` | Placeholders at the well-known paths, the not-found page, and the redirects from `/` and `/w/$ws`. |
| `proof-page.tsx` | The data layer's proof page, a dev-only route at `/dev/proof`. |

## Registering a feature

Each feature folder exports `feature` from its `index.ts`. `src/router.tsx` passes the three of
them to `createAppRouter`; nothing else in the shell names a feature.

```ts
// src/projects/index.ts
import { createRoute, lazyRouteComponent } from '@tanstack/react-router';
import { lazy } from 'react';
import { FolderIcon } from '../design/index.ts';
import { defineFeature } from '../shell/index.ts';

export const feature = defineFeature({
  id: 'projects',
  layout: 'projects',
  routes: (parent) => [
    createRoute({
      getParentRoute: () => parent,
      path: 'inbox',
      staticData: { layout: 'both', title: 'Inbox' },
      component: lazyRouteComponent(() => import('./inbox-page.tsx'), 'InboxPage'),
    }),
    createRoute({
      getParentRoute: () => parent,
      path: 'projects/$project',
      component: lazyRouteComponent(() => import('./project-page.tsx'), 'ProjectPage'),
    }),
  ],
  nav: [{ id: 'overview', label: 'Overview', to: 'overview', section: 'Workspace', icon: FolderIcon }],
  commands: [{ id: 'board', label: 'Open the board', run: (c) => c.go('board') }],
  create: [{ id: 'task', label: 'Task', dialog: lazy(() => import('./new-task.tsx')) }],
});
```

A `Feature` has:

| Field | What |
|---|---|
| `id` | Unique: `console`, `projects`, `onboarding`. |
| `layout` | `'projects'`, `'console'` or `'both'`: where its routes, entries and commands belong unless they say otherwise. |
| `routes(parent)` | Its TanStack Router subtree under `/w/$ws`. Called once per router with the workspace route as `parent` (so tests can build several routers). |
| `nav` | Sidebar entries (below). |
| `commands` | Palette commands (below). |
| `create` | "+ New" items (below). |
| `sidebar` | A component shown in the sidebar under the navigation while the feature's layout is on (the console's filters, say). Lazy-load it if it is heavy. |

### Rules

- **Keep `index.ts` light.** It is imported by `src/router.tsx`, so everything it imports statically
  is in the initial bundle (budget: 250 kB gzipped, checked by `pnpm size` after a build). Load
  pages with `lazyRouteComponent(() => import('./page.tsx'), 'Page')` and heavy dialogs and sidebar
  panels with `React.lazy`; the shell adds the Suspense boundaries.
- **Read data with the data layer's hooks** (`src/data/README.md`): `useLiveQuery`, never `useQuery`.
- **Ids are unique across features** (features, nav entries, commands, "+ New" items). A duplicate
  throws when the router is created.
- **Shortcuts** Ctrl . (layout), Ctrl K (palette), Ctrl J (Orchestrator) and Ctrl B (sidebar) are
  the shell's, with Cmd on macOS. Bind your own on your own elements, not on `window`. See below
  for keeping the shell's out of your way.
- **Tests** go in your folder, as `*.test.ts(x)` (for example `src/projects/tests/board.test.tsx`).
  Vitest picks them up, and `tsconfig.node.json` checks them with Node's types, so they can import
  `tests/hub-process.ts` (a mock hub in a child process, for tests under happy-dom) by relative path.

## Keys: claiming them from the shell

The shell listens for its shortcuts on `window`, and stays out of the way of whatever has focus:

| Focus is… | Ctrl K | Ctrl J, Ctrl B, Ctrl . |
|---|---|---|
| anywhere else | palette | Orchestrator, sidebar, layout |
| in an editable element (`input`, `textarea`, `select`, contenteditable) | palette | go to the element |
| in or under a **key-owning surface** | goes to the surface | go to the surface |

A key-owning surface is any element with `data-shell-keys="none"`; everything inside it keeps
every key, and the shell prevents nothing there. Mark a terminal, a code editor or anything else
that needs Ctrl K, J, B or . with it:

```tsx
import { ownsShellKeys } from '../shell/index.ts';

<div {...ownsShellKeys} className="terminal">…</div>   // = data-shell-keys="none"
```

`SHELL_KEYS_ATTRIBUTE` is the attribute's name. A key the shell handles is not passed on to the
browser (Ctrl J would open downloads), and holding it down toggles nothing after the first press.
A key the shell does not handle is never prevented.

## Routes, paths and layouts

Every route lives under `/w/$ws`. The shell serves placeholders at these well-known paths so the
app is navigable before the features land. **A feature route whose path matches one replaces it**
(params match by position: `projects/$projectId` replaces `projects/$project`). Links anywhere may
rely on these paths; build them with `paths` from `index.ts`.

| Path under `/w/$ws` | `paths.` | Layout | Filled by |
|---|---|---|---|
| `home` | `home(ws)` | projects | N |
| `inbox` | `inbox(ws)` | both | N |
| `my-tasks` | `myTasks(ws)` | projects | N |
| `projects/$project` | `project(ws, id)` | projects | N |
| `projects/$project/workstreams/$workstream` | `workstream(ws, project, id)` | projects | N |
| `workstreams/$workstream` (redirects to the path above) | `workstreamById(ws, id)` | projects | shell |
| `tasks/$task` (key or id) | `task(ws, key)` | projects | N |
| `console` | `console(ws)` | console | M |
| `console/$session` | `session(ws, id)` | console | M |

- **Param names** `$project`, `$workstream`, `$task` and `$session` feed the breadcrumb and the
  sidebar's current item; use them.
- **`staticData.layout`** says which layout a route belongs to; a feature's top-level routes get the
  feature's `layout` when they do not set one, and children inherit it. On a route of one layout,
  that layout is on; on a `both` route (the Inbox) the workspace's stored layout stays.
- **`staticData.title`** names the page in the breadcrumb when no param does ("Board").
- `/` redirects to the hub's workspace; `/w/$ws` redirects to the stored layout's last page (or its
  home: `home` for Projects, `console` for the Agent console). Unknown paths show a not-found page
  inside the frame.
- In components, `useWorkspaceId()` gives `$ws`, `useLayout()` the layout on screen, and
  `useParams({ strict: false })` the rest (feature routes are typed loosely: they are composed at
  run time).

## Sidebar entries

```ts
{ id: 'machines', label: 'Machines', to: 'console/machines', icon: ConsoleIcon,
  section: 'Console', badge: MachinesDown, layout: 'console', order: 50 }
```

- `to` is a path under the workspace, without a leading slash; the link is active on it and below.
- Entries without a `section` join the top list (Home 10, Inbox 20, My tasks 30, Agent console
  40); sections follow in the order they first appear, then the Projects tree (Projects layout).
- `badge` is a component rendered after the label, inside the providers, so it can call data hooks.
  Use the design `Badge` with a `label` for screen readers: `<Badge tone="accent" label="open asks">3</Badge>`.
- Entries show only in their layout (`layout`, default the feature's). In the collapsed rail only
  the icon shows, with the label as its name and tooltip.

## Palette commands

```ts
{ id: 'dispatch', label: 'Dispatch a task to an agent', group: 'Agents',
  keywords: ['run', 'start'], keys: ['mod', 'd'], run: (c) => c.create('agent') }
```

`run` gets a `CommandContext`: `workspace`, `go(path)` (under the workspace), `switchLayout(layout)`
and `create(id)`. Commands outside the current layout are hidden. The palette also searches
projects, workstreams, tasks (by key and title) and sessions from the live query cache.

## "+ New"

The shell has placeholder dialogs for `task`, `agent`, `project` and `team`. A feature's
`CreateEntry` with the same `id` replaces the placeholder; other ids add items. `dialog` is the
body (it gets `close()`); the shell supplies the modal, its title (`title`, default "New
<label>"), focus handling and a Suspense boundary. The palette lists every item as "New …".

## Wiring components that already exist

- **Merging:** keep your `index.ts` exports and add `export const feature = defineFeature({ … })`
  from `../shell/index.ts` (the stub is one line).
- **Projects (N):** give `ProjectsNavProvider` handlers built on `paths` and `useRouter().navigate({ href })`:
  `openTask` → `paths.task`, `openProject` → `paths.project`, `openWorkstream` →
  `paths.workstreamById`, `openSession` → `paths.session`, `openInbox` → `paths.inbox`. The
  shell's Inbox badge uses the same query key as `useInbox`, so they share one fetch.
- **Console (M):** `console` and `console/$session` are the list and the session; `SessionFilters`
  fits the `sidebar` slot of a `layout: 'console'` feature.

## Testing a feature against the shell

`tests/shell.test.tsx` renders `createAppRouter([stub], { history: createMemoryHistory(…) })`
inside a `DataProvider` pointed at a mock hub on a free port, checks the stub's nav entry appears
and routes, and that a feature route replaces a placeholder. Copy it for your feature.
