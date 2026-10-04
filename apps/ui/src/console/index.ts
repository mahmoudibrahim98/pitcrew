// The Agent console's public surface. `feature` registers it with the shell (src/shell/README.md):
// the routes `console` and `console/$session`, and palette commands. The app loads
// this file at start, so it stays small: the page and every component are lazy chunks (render the
// components inside a <Suspense>), and markdown and diff parsing run in their own worker chunk.

import { createRoute, lazyRouteComponent, type AnyRoute } from '@tanstack/react-router';
import { lazy } from 'react';
import { defineFeature, type Command, type WorkspaceRoute } from '../shell/index.ts';
import { requestConsole, type ConsoleIntent } from './intent.ts';
import { WORKBENCH_ACTIONS, type WorkbenchAction } from './workbench/keys.ts';

function routes(parent: WorkspaceRoute): AnyRoute[] {
  // One page for both paths: the list stays put while the chosen session changes.
  const consoleRoute = createRoute({
    getParentRoute: () => parent,
    path: 'console',
    staticData: { layout: 'console', title: 'Agent console' },
    component: lazyRouteComponent(() => import('./console-page.tsx'), 'ConsolePage'),
  });
  return [
    consoleRoute.addChildren([
      createRoute({ getParentRoute: () => consoleRoute, path: '/' }),
      createRoute({ getParentRoute: () => consoleRoute, path: '$session' }),
    ]),
  ];
}

const ask =
  (intent: ConsoleIntent): Command['run'] =>
  (context) =>
    requestConsole(context, intent);

const GROUP = 'Agent console';

const commands: Command[] = [
  {
    id: 'console-open',
    label: 'Go to the Agent console',
    group: 'Go to',
    keywords: ['open', 'console', 'sessions', 'agents'],
    run: (context) => context.go('console'),
  },
  {
    id: 'console-jump',
    label: 'Jump to a session…',
    group: GROUP,
    keywords: ['switch', 'pick', 'session list', 'agents'],
    run: ask({ kind: 'list' }),
  },
  {
    id: 'console-filter-machine',
    label: 'Filter sessions by machine…',
    group: GROUP,
    keywords: ['host', 'computer', 'cluster'],
    run: ask({ kind: 'facet', facet: 'machine' }),
  },
  {
    id: 'console-filter-state',
    label: 'Filter sessions by state…',
    group: GROUP,
    keywords: ['status', 'working', 'waiting', 'idle', 'ended'],
    run: ask({ kind: 'facet', facet: 'state' }),
  },
  {
    id: 'console-waiting',
    label: 'Show sessions waiting for input',
    group: GROUP,
    keywords: ['needs me', 'blocked', 'question', 'filter'],
    run: ask({ kind: 'state', state: 'waiting' }),
  },
  {
    id: 'console-working',
    label: 'Show working sessions',
    group: GROUP,
    keywords: ['running', 'busy', 'filter'],
    run: ask({ kind: 'state', state: 'working' }),
  },
  {
    id: 'console-clear-filters',
    label: 'Clear the session filters',
    group: GROUP,
    keywords: ['reset', 'all sessions'],
    run: ask({ kind: 'clear' }),
  },
];

/** The workbench's keys, each as a command (shown in the console's layout only). */
const workbenchCommands: Command[] = (Object.keys(WORKBENCH_ACTIONS) as WorkbenchAction[]).map((action) => {
  const { label, keys, keywords } = WORKBENCH_ACTIONS[action];
  return {
    id: `console-workbench-${action}`,
    label,
    group: 'Workbench',
    ...(keys === undefined ? {} : { keys }),
    ...(keywords === undefined ? {} : { keywords }),
    layout: 'console',
    run: ask({ kind: 'workbench', action }),
  };
});

export const feature = defineFeature({
  id: 'console',
  layout: 'console',
  routes,
  // The shell's own "Agent console" entry (with its working and waiting counts) is the console's
  // place in the sidebar; the console adds none of its own. No "+ New" item yet: starting a
  // session needs a flow that does not exist, and an entry cannot be shown disabled with a reason.
  commands: [...commands.map((command): Command => ({ ...command, layout: 'both' })), ...workbenchCommands],
});

export const SessionList = lazy(() => import('./session-list.tsx').then((m) => ({ default: m.SessionList })));
export const SessionListView = lazy(() =>
  import('./session-list.tsx').then((m) => ({ default: m.SessionListView })),
);
export const SessionFilters = lazy(() =>
  import('./session-filters.tsx').then((m) => ({ default: m.SessionFilters })),
);
export const ChatView = lazy(() => import('./chat-view.tsx').then((m) => ({ default: m.ChatView })));
export const Composer = lazy(() => import('./composer.tsx').then((m) => ({ default: m.Composer })));
export const QuestionCard = lazy(() => import('./question-card.tsx').then((m) => ({ default: m.QuestionCard })));
export const SessionHeader = lazy(() =>
  import('./session-header.tsx').then((m) => ({ default: m.SessionHeader })),
);
/** A session's live terminal; xterm.js is in this chunk alone. */
export const TerminalView = lazy(() =>
  import('./terminal/terminal-view.tsx').then((m) => ({ default: m.TerminalView })),
);

export { hasFacets, NO_FACETS, UNSORTED, type SessionFacets } from './facets.ts';
export { OpenExternalProvider } from './render/links.tsx';
export type { ChatViewProps } from './chat-view.tsx';
export type { ComposerProps } from './composer.tsx';
export type { QuestionCardProps } from './question-card.tsx';
export type { SessionFiltersProps } from './session-filters.tsx';
export type { SessionHeaderProps } from './session-header.tsx';
export type { SelectVia, SessionListProps, SessionListViewProps } from './session-list.tsx';
export type { TerminalViewProps } from './terminal/terminal-view.tsx';
export type { TerminalSocketFactory } from './terminal/socket.ts';
