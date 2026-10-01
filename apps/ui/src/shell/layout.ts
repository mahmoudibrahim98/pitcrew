// Which layout is on screen, and switching between them.
//
// A route belongs to one layout or to both (its `staticData.layout`). On a route of one layout,
// that layout is on; on a route of both (the Inbox), the workspace's stored choice is. Switching
// goes to where the other layout was last left, or to its home, and stores the choice.

import { useParams, useRouterState, type AnyRouter } from '@tanstack/react-router';
import { useEffect } from 'react';
import type { LayoutId, LayoutScope } from './feature.ts';
import { paths } from './paths.ts';
import { lastPath, storedLayout, useShell } from './store.ts';

export const LAYOUTS: Record<LayoutId, { label: string; home: string }> = {
  projects: { label: 'Projects', home: 'home' },
  console: { label: 'Agent console', home: 'console' },
};

interface MatchLike {
  staticData?: { layout?: LayoutScope | undefined } | undefined;
  status?: string;
}

/** The deepest route's layout, if any route on screen declares one. */
export function layoutOfMatches(matches: readonly MatchLike[]): LayoutScope | undefined {
  for (let i = matches.length - 1; i >= 0; i -= 1) {
    const layout = matches[i]?.staticData?.layout;
    if (layout !== undefined) return layout;
  }
  return undefined;
}

function resolve(route: LayoutScope | undefined, stored: LayoutId): LayoutId {
  return route === 'projects' || route === 'console' ? route : stored;
}

/** The workspace id from the URL (`/w/$ws/…`), or '' outside a workspace. */
export function useWorkspaceId(): string {
  const params: { ws?: string } = useParams({ strict: false });
  return params.ws ?? '';
}

export function useLayout(): LayoutId {
  const ws = useWorkspaceId();
  const route = useRouterState({ select: (s) => layoutOfMatches(s.matches) });
  const stored = useShell((s) => s.workspaces[ws]?.layout ?? 'projects');
  return resolve(route, stored);
}

export function currentLayout(router: AnyRouter, ws: string): LayoutId {
  return resolve(layoutOfMatches(router.state.matches), storedLayout(ws));
}

/** Switches to `target` (or the other layout), at the page it was last left on. */
export function switchLayout(router: AnyRouter, ws: string, target?: LayoutId): void {
  const current = currentLayout(router, ws);
  const next = target ?? (current === 'projects' ? 'console' : 'projects');
  useShell.getState().setLayout(ws, next);
  if (next === current) return;
  router.history.push(lastPath(ws, next) ?? paths.under(ws, LAYOUTS[next].home));
}

/** Remembers the page on screen as the last one of its layout. Rendered once, in the frame. */
export function LayoutMemory() {
  const ws = useWorkspaceId();
  // The resolved location, not the pending one: it is the one the matches belong to.
  const href = useRouterState({ select: (s) => s.resolvedLocation?.href });
  const route = useRouterState({ select: (s) => layoutOfMatches(s.matches) });
  const found = useRouterState({
    select: (s) => s.matches.every((m) => m.status !== 'notFound' && m.status !== 'error'),
  });
  const idle = useRouterState({ select: (s) => s.status === 'idle' });
  const remember = useShell((s) => s.remember);

  useEffect(() => {
    if (!idle || ws === '' || href === undefined || route === undefined || !found) return;
    remember(ws, route === 'both' ? storedLayout(ws) : route, href);
  }, [idle, ws, href, route, found, remember]);

  return null;
}
