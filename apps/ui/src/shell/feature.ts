// The feature registration interface. Each feature folder (console, projects, onboarding) exports
// `feature` from its index.ts; src/router.tsx hands them to the shell, which composes their
// routes, sidebar entries, palette commands and "+ New" items. See README.md in this folder.

import type { AnyRoute } from '@tanstack/react-router';
import type { ComponentType } from 'react';
import type { ShellRootRoute, WorkspaceRoute } from './routes.tsx';

/** The two layouts over the same data (ADR-0008). */
export type LayoutId = 'projects' | 'console';
/** Where something shows: one layout, or both. */
export type LayoutScope = LayoutId | 'both';

declare module '@tanstack/react-router' {
  interface StaticDataRouteOption {
    /** The layout the route belongs to. A feature's routes default to the feature's layout. */
    layout?: LayoutScope;
    /** The page's name in the breadcrumb when no route param names it ("Inbox", "Board"). */
    title?: string;
    /**
     * The route serves a workspace that is not set up yet (the first-run wizard, at
     * `paths.setup`). The shell sends a workspace with `setup_needed` to `paths.setup` from every
     * other route, but not from this one; shows it without the sidebar and top bar; and never
     * remembers it as a layout's last page.
     */
    setup?: boolean;
  }
}

/** An entry in the sidebar. */
export interface NavEntry {
  /** Unique across all features. */
  id: string;
  label: string;
  /** A path under the workspace, without a leading slash: `inbox`, `projects/overview`. */
  to: string;
  icon?: ComponentType<{ className?: string | undefined }>;
  /** A heading to group under. Entries without one go in the top list, with Home and Inbox. */
  section?: string;
  /**
   * Rendered after the label: a count or a status, usually a `Badge` fed by a data hook. It runs
   * inside the data and router providers, so it may call `useLiveQuery` hooks.
   */
  badge?: ComponentType;
  /** Defaults to the feature's layout. */
  layout?: LayoutScope;
  /** Lower comes first. The shell's own entries use 10 to 40. Default 100. */
  order?: number;
}

/** What a palette command gets when it runs. */
export interface CommandContext {
  /** The current workspace's id. */
  workspace: string;
  /** Navigates to a path under the workspace, e.g. `go('inbox')`. */
  go(path: string): void;
  /** Switches layout, going to where that layout was last left. */
  switchLayout(layout: LayoutId): void;
  /** Opens a "+ New" dialog by its id (`task`, `agent`, `project`, `team`, or a feature's own). */
  create(id: string): void;
}

/** A command in the palette (Ctrl K). */
export interface Command {
  /** Unique across all features. */
  id: string;
  label: string;
  /** Shown beside the label; default "Command". */
  group?: string;
  /** Extra words the fuzzy search matches. */
  keywords?: readonly string[];
  /** A shortcut hint, e.g. `['mod', '.']`. The shell does not bind it; bind it yourself. */
  keys?: readonly string[];
  /** Defaults to the feature's layout. Commands outside the current layout are hidden. */
  layout?: LayoutScope;
  run(context: CommandContext): void;
}

/** An item in the "+ New" menu. */
export interface CreateEntry {
  /** `task`, `agent`, `project` and `team` replace the shell's placeholders. */
  id: string;
  label: string;
  /** The dialog's title; default "New <label>". */
  title?: string;
  /** The dialog body. Wrap a heavy form in `React.lazy`; the shell adds the Suspense boundary. */
  dialog: ComponentType<{ close(): void }>;
  /** Lower comes first. The shell's own items use 10 to 40. Default 100. */
  order?: number;
  /**
   * Shows the item disabled, with this as its reason. The item stays focusable (so the reason is
   * reachable from the keyboard) but selecting it does nothing; the palette hides it.
   */
  disabled?: string;
}

export interface Feature {
  /** Unique: `console`, `projects`, `onboarding`. */
  id: string;
  /** The layout the feature's routes, entries and commands belong to unless they say otherwise. */
  layout: LayoutScope;
  /**
   * Routes under `/w/$ws`, as a TanStack Router subtree. Called once per router with the workspace
   * route as the parent: `createRoute({ getParentRoute: () => parent, path: 'inbox', … })`.
   * Load page components lazily (`lazyRouteComponent`) so each feature is its own chunk. A route
   * whose path matches one of the shell's placeholders (see README.md) replaces it.
   */
  routes?: (parent: WorkspaceRoute) => AnyRoute[];
  /**
   * Routes outside any workspace, for what runs before there is one (the desktop's "connect a
   * remote machine" wizard, at `paths.connect()`). Called once per router with the root route as
   * the parent. They render without the frame and without a workspace's data; lazy-load them.
   */
  rootRoutes?: (root: ShellRootRoute) => AnyRoute[];
  nav?: readonly NavEntry[];
  commands?: readonly Command[];
  create?: readonly CreateEntry[];
  /**
   * Content for the sidebar below the navigation, shown while the feature's layout is on (the
   * console's filters, for example). Lazy-load it if it is heavy.
   */
  sidebar?: ComponentType;
}

/** Identity, for type checking: `export const feature = defineFeature({ … })`. */
export function defineFeature(feature: Feature): Feature {
  return feature;
}

export function inLayout(scope: LayoutScope, layout: LayoutId): boolean {
  return scope === 'both' || scope === layout;
}
