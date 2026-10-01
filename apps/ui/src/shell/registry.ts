// Composes the shell's own feature and the registered features into one registry: the sidebar
// entries, palette commands, "+ New" items and sidebar panels, each with its layout resolved.

import type { AnyRoute } from '@tanstack/react-router';
import type { ComponentType } from 'react';
import type { Command, CreateEntry, Feature, LayoutScope, NavEntry } from './feature.ts';

export interface ResolvedNav extends NavEntry {
  layout: LayoutScope;
  order: number;
  feature: string;
}

export interface ResolvedCommand extends Command {
  layout: LayoutScope;
  feature: string;
}

export interface ResolvedCreate extends CreateEntry {
  order: number;
  feature: string;
}

export interface SidebarPanel {
  feature: string;
  layout: LayoutScope;
  component: ComponentType;
}

export interface Registry {
  /** The shell's own feature first, then the registered ones in order. */
  features: readonly Feature[];
  nav: readonly ResolvedNav[];
  commands: readonly ResolvedCommand[];
  create: readonly ResolvedCreate[];
  sidebars: readonly SidebarPanel[];
}

function assertUnique(ids: readonly string[], what: string): void {
  const seen = new Set<string>();
  for (const id of ids) {
    if (seen.has(id)) throw new Error(`Two features register the ${what} "${id}"`);
    seen.add(id);
  }
}

const byOrder = (a: { order: number }, b: { order: number }) => a.order - b.order;

export function composeFeatures(shell: Feature, features: readonly Feature[]): Registry {
  const all = [shell, ...features];
  assertUnique(
    all.map((f) => f.id),
    'feature',
  );

  const nav = all
    .flatMap((f) =>
      (f.nav ?? []).map((e) => ({ ...e, layout: e.layout ?? f.layout, order: e.order ?? 100, feature: f.id })),
    )
    .sort(byOrder);
  assertUnique(
    nav.map((e) => e.id),
    'nav entry',
  );

  const commands = all.flatMap((f) =>
    (f.commands ?? []).map((c) => ({ ...c, layout: c.layout ?? f.layout, feature: f.id })),
  );
  assertUnique(
    commands.map((c) => c.id),
    'command',
  );

  // A feature's item replaces the shell's placeholder with the same id.
  const create = new Map<string, ResolvedCreate>();
  for (const e of shell.create ?? []) create.set(e.id, { ...e, order: e.order ?? 100, feature: shell.id });
  assertUnique(
    features.flatMap((f) => (f.create ?? []).map((e) => e.id)),
    '"+ New" item',
  );
  for (const f of features) {
    for (const e of f.create ?? []) {
      create.set(e.id, { ...e, order: e.order ?? create.get(e.id)?.order ?? 100, feature: f.id });
    }
  }

  const sidebars = all.flatMap((f) =>
    f.sidebar === undefined ? [] : [{ feature: f.id, layout: f.layout, component: f.sidebar }],
  );

  return { features: all, nav, commands, create: [...create.values()].sort(byOrder), sidebars };
}

function childrenOf(route: AnyRoute): AnyRoute[] {
  const children: unknown = route.children;
  if (Array.isArray(children)) return children as AnyRoute[];
  if (typeof children === 'object' && children !== null) return Object.values(children) as AnyRoute[];
  return [];
}

function normalise(path: string): string {
  return path
    .split('/')
    .filter((part) => part !== '')
    .map((part) => (part.startsWith('$') ? '$' : part))
    .join('/');
}

/**
 * Every path a route subtree serves, relative to its parent, with params written `$`
 * (`projects/$/workstreams/$`). Pathless layout routes add no segment.
 */
export function servedPaths(route: AnyRoute, prefix = ''): string[] {
  const own = (route.options as { path?: unknown } | undefined)?.path;
  const full = typeof own === 'string' ? normalise(`${prefix}/${own}`) : normalise(prefix);
  const nested = childrenOf(route).flatMap((child) => servedPaths(child, full));
  return typeof own === 'string' ? [full, ...nested] : nested;
}

/** Gives each top-level route the layout it lacks, so its whole subtree inherits one. */
export function withLayout(routes: AnyRoute[], layout: LayoutScope): AnyRoute[] {
  for (const route of routes) {
    const options = route.options as { staticData?: { layout?: LayoutScope } };
    if (options.staticData?.layout === undefined) {
      options.staticData = { ...options.staticData, layout };
    }
  }
  return routes;
}
