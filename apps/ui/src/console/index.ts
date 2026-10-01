// The Agent console's public surface. Importing this file loads no console code: each component
// is a lazy chunk fetched on first render (render them inside a <Suspense>), and markdown and
// diff parsing run in their own worker chunk. `feature` registers the console with the shell (its
// interface is in src/shell/README.md); stream M's wiring brief fills it in.

import { lazy } from 'react';
import { defineFeature } from '../shell/index.ts';

export const feature = defineFeature({ id: 'console', layout: 'console' });

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

export { hasFacets, NO_FACETS, UNSORTED, type SessionFacets } from './facets.ts';
export { OpenExternalProvider } from './render/links.tsx';
export type { ChatViewProps } from './chat-view.tsx';
export type { ComposerProps } from './composer.tsx';
export type { QuestionCardProps } from './question-card.tsx';
export type { SessionFiltersProps } from './session-filters.tsx';
export type { SessionHeaderProps } from './session-header.tsx';
export type { SessionListProps, SessionListViewProps } from './session-list.tsx';
export type { TranscriptItem, TranscriptPage } from './types.ts';
