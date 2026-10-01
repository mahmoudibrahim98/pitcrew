// Stream N's public surface: the Projects layout feature, and its components. The components are
// lazy, so the projects code loads only when one is shown; render them inside <Suspense>.
//
// `feature` is still stream L's stub (the shell's registration interface is in
// src/shell/README.md); the routes that use these components come with the next N brief.

import { lazy } from 'react';
import { defineFeature } from '../shell/index.ts';

export const feature = defineFeature({ id: 'projects', layout: 'projects' });

export const Home = lazy(() => import('./home.tsx').then((m) => ({ default: m.Home })));
export const Inbox = lazy(() => import('./inbox.tsx').then((m) => ({ default: m.Inbox })));
export const Board = lazy(() => import('./board.tsx').then((m) => ({ default: m.Board })));
export const TaskDrawer = lazy(() => import('./task-drawer.tsx').then((m) => ({ default: m.TaskDrawer })));
export const TaskDetail = lazy(() => import('./task-drawer.tsx').then((m) => ({ default: m.TaskDetail })));
export const WhereItStands = lazy(() => import('./where-it-stands.tsx').then((m) => ({ default: m.WhereItStands })));
export const ProjectOverview = lazy(() => import('./overview.tsx').then((m) => ({ default: m.ProjectOverview })));
export const WorkstreamOverview = lazy(() =>
  import('./overview.tsx').then((m) => ({ default: m.WorkstreamOverview })),
);
export const WorkstreamsTable = lazy(() => import('./overview.tsx').then((m) => ({ default: m.WorkstreamsTable })));
export const ActivityFeed = lazy(() => import('./activity.tsx').then((m) => ({ default: m.ActivityFeed })));
export const AgentsNow = lazy(() => import('./agents.tsx').then((m) => ({ default: m.AgentsNow })));
/** Local stand-in for stream M's console `QuestionCard`. */
export const QuestionCard = lazy(() => import('./question-card.tsx').then((m) => ({ default: m.QuestionCard })));

export { ProjectsNavProvider, useProjectsNav, type ProjectsNav } from './nav.tsx';
/** Open asks to me, for the sidebar's Inbox badge. */
export { useInbox } from './data.ts';
export type { BoardGrouping, BoardProps } from './board.tsx';
