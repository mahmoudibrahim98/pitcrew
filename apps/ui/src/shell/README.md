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
| `frame.tsx`, `sidebar.tsx`, `projects-tree.tsx`, `top-bar.tsx`, `orchestrator.tsx`, `create.tsx` | The frame's parts. The frame sits in the workspace's data scope (`WorkspaceScope`, keyed by `$ws`). |
| `palette.tsx`, `fuzzy.ts` | The palette (a lazy chunk) and its fuzzy matching. |
| `pages/` | Placeholders at the well-known paths, the not-found page, the redirects from `/` and `/w/$ws`, and `unavailable.tsx` (a workspace the desktop gateway cannot reach, or that needs pairing). |
| `proof-page.tsx` | The data layer's proof page, a dev-only route at `/dev/proof`. |
| `gateway-navigate.ts` | `gateway://navigate` (deep links, the app's own notifications), in the desktop only: `parseNavigateTarget` and `navigateHref` are the pure checks, `useGatewayNavigation()` wires them to the router. See "Navigating from outside the window" below. |
| `notice.tsx` | `<Notice>`: a brief, dismissible message with nowhere better to show (an unknown-workspace deep link, today), from `useShell`'s `notice`. Mounted once, at the root, above `/`'s redirect. |
| `prompts.tsx`, `prompt-dialog.tsx` | SSH's prompts, in the desktop only: `<GatewayPrompts>` is mounted once, at the root, and shows the oldest prompt in `<PromptDialog>` (a lazy chunk). See "SSH's prompts" below. |
| `remove-workspace.tsx` | "Remove workspace…" (remote workspaces, desktop only): the confirmation, with "Also stop PitCrew on the remote (cancels its SLURM job)". A lazy chunk. |

## Workspaces in the desktop app

In a browser the hub has one workspace. In the desktop app (`src/data/README.md`, "Workspaces in
the desktop app"):

- the switcher lists the gateway's workspaces, with their state when it is not `ready`
  ("Beta Lab · Unreachable"), and follows `gateway://workspaces`;
- each workspace has its own data scope: switching remounts the frame, and never shows another
  workspace's data;
- a workspace that is `unreachable` or `needs_pairing` keeps the frame (so another is one click
  away) and shows why in the main area, with the gateway's `detail`; the top bar's pill says so.
  A remote one that is `unreachable` (a sign-in cancelled while reconnecting, say) offers Retry
  (`gateway_workspace_retry`);
- a workspace the gateway drops while it is on screen (removed, here or elsewhere) is left for the
  next ready one, or `/`; one never listed while on screen stays a not-found page;
- `/` opens the workspace last opened, or the first ready one; an id the gateway does not list
  is not found;
- a workspace out of view for 10 minutes has its stream closed, resumed with `since` when it is
  opened again (`src/data/README.md`, "Workspaces in the desktop app");
- the switcher ends with "Connect a remote machine…", which opens `paths.connect()` (the onboarding
  feature's wizard, a root route), and, when the workspace on screen is remote, "Remove
  workspace…", which asks first (optionally stopping the remote's helper, which cancels its SLURM
  job), then opens another workspace. In a browser the connect item is disabled and says the
  desktop app is needed; there is nothing to remove;
- with no workspace at all, `/` says "No workspaces yet." and offers the same connect action.

## The first run: `setup_needed`

A hub with no person yet answers `GET /v1/workspace` with `setup_needed: true` (api-v1.md, "The
first run"). The frame sends such a workspace to `paths.setup(ws)` (`/w/$ws/onboarding`, the
onboarding feature's first-run wizard), in the browser and in the desktop app, from whatever page
was asked for. There is no loop:

- the routes that set a workspace up say so with `staticData.setup: true`, and are exempt (the
  shell's own placeholder at that path too, so the redirect always lands somewhere);
- a finished setup flips `setup_needed` off in the cache at once (`src/data/setup.ts`), before the
  wizard navigates to Home.

A `setup` route is shown bare, without the sidebar, top bar or Orchestrator, and is never
remembered as a layout's last page. In the desktop app it keeps a small header with the workspace
switcher, so another workspace, connecting a machine, or removing this one (a remote) stays one
click away. A workspace is remembered as the last one opened only once it is set up (or when the
gateway cannot reach it), so `/` does not keep reopening a workspace waiting for setup.

## SSH's prompts

Any gateway command that talks to a remote, and a connection reconnecting, may make SSH ask for a
password, a key's passphrase, a one-time code, or a new host key's confirmation
(desktop-gateway.md, "Prompts"). `<GatewayPrompts>` (mounted once, in `routes.tsx`'s root) shows
the oldest one; the rest wait, and the dialog says how many.

- It says which host is asking and where an answer goes, from `kind` (never guessed from the
  text): "Sent to {host}" for a password or a code, "Unlocks your key on this computer; not sent to
  {host}" for a passphrase, "Trust this host's key?" for a host key; a `confirm` is ssh's own
  yes-or-no question, and a `notice` is information only.
- ssh's `text` is plain text (untrusted, never rendered as markup), in a box that scrolls
  (focusable) so the field and the buttons always show. A long text arrives with its end kept
  (`src/data/remote.ts`).
- A password, passphrase or code goes in a password field with autocomplete off; Send stays off
  while it is empty. A host key shows its fingerprint, with Reject and Accept (Accept stays off
  when the gateway gave no fingerprint to compare); a `confirm` has Reject and Accept. A `notice`
  ("touch your security key") has nothing to answer: it stays until the gateway withdraws it, has
  no close button, ignores Esc, and only "Stop sign-in" stops ssh.
- Every other kind has "Cancel sign-in", which replies with neither field; Esc does the same, and
  the button says so (`aria-keyshortcuts`). A click outside the dialog does nothing. A
  `gateway://prompt-closed` withdraws the prompt without a reply.
- Enter does nothing for 300 ms after the dialog opens, so typing meant for another field cannot
  send a half-typed password.
- **The answer is never in state or in the DOM.** The field is uncontrolled (React copies a
  controlled input's value into its `value` attribute, and so into `outerHTML`, snapshots and
  traces); the answer is read from it once, as it is sent, and the field is cleared at that
  moment. Each prompt gets a fresh field (keyed by its id). It is never logged or put in a store,
  a query cache, the URL or storage (`tests/prompts.test.tsx` looks for it everywhere).

## Navigating from outside the window

`gateway://navigate` (`docs/build/contracts/desktop-gateway.md`), in the desktop only: a deep link
or a click on the app's own notifications gives a typed `NavigateTarget`
(`{ workspace, kind, id? }`), already shape-checked by the gateway. `gateway-navigate.ts` checks it
again — `parseNavigateTarget` (the workspace is a ULID, `kind` is one of the five, `id` is required
unless `kind` is `inbox`) — and maps it to a route with `paths.inbox`/`task`/`session`/`project`/
`workstreamById` (`navigateHref`), never by treating a field as a URL or a path to concatenate.
Anything that does not parse is dropped and logged, shortened; a well-formed target whose workspace
the gateway does not currently list goes to `/` with a notice (`store.ts`'s `notice`, shown by
`<Notice>`) instead of guessing a path for it. `useGatewayNavigation()` wires this to the router and
is mounted once, in `routes.tsx`'s root component.

A target that arrives at launch, before the gateway's workspace list is first known, is never
checked against a stale (empty) list: the data layer holds it — up to 60 seconds, the contract's
own limit, after which it is dropped — and only calls `useGatewayNavigate`'s listener once the list
has arrived, with that list (`src/data/README.md`, `desktop.tsx`).

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
| `rootRoutes(root)` | Routes outside any workspace, under `/`, for what runs before there is one (the onboarding feature's "connect a remote machine" wizard at `/connect`). They render without the frame or a workspace's data. |
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
| `onboarding` (`staticData.setup`) | `setup(ws)` | both | O |
| `/connect` (a root route, outside `/w/$ws`) | `connect()` | none | O |

- **Param names** `$project`, `$workstream`, `$task` and `$session` feed the breadcrumb and the
  sidebar's current item; use them.
- **`staticData.layout`** says which layout a route belongs to; a feature's top-level routes get the
  feature's `layout` when they do not set one, and children inherit it. On a route of one layout,
  that layout is on; on a `both` route (the Inbox) the workspace's stored layout stays.
- **`staticData.title`** names the page in the breadcrumb when no param does ("Board").
- **`staticData.setup`** marks a route that sets a workspace up (see "The first run" above).
- `/` redirects to the hub's workspace (in the desktop app, see above); `/w/$ws` redirects to the stored layout's last page (or its
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

An entry with `disabled` (a reason) shows in the menu disabled, with that reason as an accessible
description; it stays focusable, so the reason is reachable from the keyboard, but selecting it
does nothing, and the palette leaves it out entirely:

```ts
{ id: 'session', label: 'Session', dialog: NewSession, disabled: 'Starting a session is not available yet.' }
```

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

Creation entries now come from their owning features; the shell has no placeholder create forms.
`CreateEntry.projectContext` restricts an entry to routes with a project parameter in both the
menu and palette. The Projects feature uses it for Workstream, and its project-page button opens
the same registered dialog. The shell retains modal trapping, Close/Escape and opener restoration.
