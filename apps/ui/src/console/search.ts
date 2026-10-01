// The session list's facets in the URL's search, so a link (or a reload) reproduces the view:
// `?machine=<id>,<id>&state=working,waiting`. Each facet is one comma-separated parameter; empty
// facets are left out. No React.

import type { Engine, SessionState } from '../data/index.ts';
import type { SessionFacets } from './facets.ts';
import { ENGINES, SESSION_STATES } from './format.ts';

export const FACET_NAMES = [
  'machine',
  'engine',
  'state',
  'project',
  'workstream',
] as const satisfies readonly (keyof SessionFacets)[];

/** Values from one search parameter: a comma-separated string, or the array a JSON link holds. */
function values(raw: unknown): string[] {
  const parts: unknown[] = Array.isArray(raw) ? raw : typeof raw === 'string' ? raw.split(',') : [];
  const seen = new Set<string>();
  for (const part of parts) {
    if (typeof part !== 'string') continue;
    const value = part.trim();
    if (value !== '') seen.add(value);
  }
  return [...seen];
}

function known<T extends string>(raw: unknown, allowed: readonly T[]): T[] {
  return values(raw).filter((v): v is T => (allowed as readonly string[]).includes(v));
}

/** The facets a parsed search holds; anything malformed is ignored. */
export function facetsFromSearch(search: Readonly<Record<string, unknown>>): SessionFacets {
  return {
    machine: values(search.machine),
    engine: known<Engine>(search.engine, ENGINES),
    state: known<SessionState>(search.state, SESSION_STATES),
    project: values(search.project),
    workstream: values(search.workstream),
  };
}

/** `search` with its facets replaced by `facets`; other parameters are kept. */
export function searchWithFacets(
  search: Readonly<Record<string, unknown>>,
  facets: SessionFacets,
): Record<string, unknown> {
  const names: readonly string[] = FACET_NAMES;
  const next = Object.fromEntries(Object.entries(search).filter(([name]) => !names.includes(name)));
  for (const name of FACET_NAMES) {
    if (facets[name].length > 0) next[name] = facets[name].join(',');
  }
  return next;
}

/** How many facet values are chosen. */
export function facetCount(facets: SessionFacets): number {
  return FACET_NAMES.reduce((sum, name) => sum + facets[name].length, 0);
}

/** What the session pane shows: the chat (the default) or the terminal (`?view=terminal`). */
export type SessionView = 'chat' | 'terminal';

export function viewFromSearch(search: Readonly<Record<string, unknown>>): SessionView {
  return search.view === 'terminal' ? 'terminal' : 'chat';
}

/** `search` showing `view`; the chat, being the default, is no parameter at all. */
export function searchWithView(search: Readonly<Record<string, unknown>>, view: SessionView): Record<string, unknown> {
  const next = Object.fromEntries(Object.entries(search).filter(([name]) => name !== 'view'));
  if (view === 'terminal') next.view = 'terminal';
  return next;
}
