// What the console's palette commands ask of the console page: show the list, open a filter, or
// filter by state. A command hands its request here; the page acts on it at once if it is on
// screen, or as it mounts after the command has gone to the console. Small and React-free: the
// commands live in index.ts, which the app loads at start.

import type { SessionState } from '../data/index.ts';
import type { CommandContext } from '../shell/index.ts';

export type ConsoleIntent =
  | { kind: 'list' }
  | { kind: 'facet'; facet: 'machine' | 'state' }
  | { kind: 'state'; state: SessionState }
  | { kind: 'clear' };

let pending: ConsoleIntent | undefined;
const listeners = new Set<() => void>();

/** Hands `intent` to the console page, going to the console first unless the page is open. */
export function requestConsole(context: CommandContext, intent: ConsoleIntent): void {
  pending = intent;
  if (listeners.size === 0) context.go('console');
  else for (const listener of listeners) listener();
}

/** The request waiting for the page, if any; taking it clears it. */
export function takeIntent(): ConsoleIntent | undefined {
  const intent = pending;
  pending = undefined;
  return intent;
}

/** Calls `listener` when a request arrives while the page is open. */
export function onIntent(listener: () => void): () => void {
  listeners.add(listener);
  return () => {
    listeners.delete(listener);
  };
}
