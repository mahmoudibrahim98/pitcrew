// Stream N's public surface: the Projects layout feature (routes, nav, commands, "+ New"), and its
// components. The components are lazy, so the projects code loads only when a projects route is
// visited; render them inside <Suspense> if you use them directly.

import { createRoute, lazyRouteComponent, type AnyRoute } from '@tanstack/react-router';
import { lazy } from 'react';
import { defineFeature, type WorkspaceRoute } from '../shell/index.ts';

/** Every route under one pathless layout, so `ProjectsNavProvider` wires up once (`layout.tsx`). */
function projectsRoutes(parent: WorkspaceRoute): AnyRoute[] {
  const projectsLayout = createRoute({
    getParentRoute: () => parent,
    id: 'projects-layout',
    component: lazyRouteComponent(() => import('./layout.tsx'), 'ProjectsLayout'),
  });
  return [
    projectsLayout.addChildren([
      createRoute({ getParentRoute: () => projectsLayout, path: 'files', staticData: { layout: 'both', title: 'Files' }, component: lazyRouteComponent(() => import('./files-page.tsx'), 'FilesPage') }),
      createRoute({
        getParentRoute: () => projectsLayout,
        path: 'home',
        staticData: { title: 'Home' },
        component: lazyRouteComponent(() => import('./home.tsx'), 'Home'),
      }),
      createRoute({
        getParentRoute: () => projectsLayout,
        path: 'inbox',
        staticData: { layout: 'both', title: 'Inbox' },
        component: lazyRouteComponent(() => import('./inbox.tsx'), 'Inbox'),
      }),
      createRoute({
        getParentRoute: () => projectsLayout,
        path: 'my-tasks',
        staticData: { title: 'My tasks' },
        component: lazyRouteComponent(() => import('./my-tasks.tsx'), 'MyTasksPage'),
      }),
      createRoute({
        getParentRoute: () => projectsLayout,
        path: 'calendar',
        staticData: { title: 'Calendar' },
        component: lazyRouteComponent(() => import('./calendar.tsx'), 'CalendarPage'),
      }),
      createRoute({
        getParentRoute: () => projectsLayout,
        path: 'projects',
        staticData: { title: 'Projects' },
        component: lazyRouteComponent(() => import('./projects-list.tsx'), 'ProjectsListPage'),
      }),
      createRoute({
        getParentRoute: () => projectsLayout,
        path: 'members',
        staticData: { title: 'Members' },
        component: lazyRouteComponent(() => import('./members.tsx'), 'MembersPage'),
      }),
      createRoute({
        getParentRoute: () => projectsLayout,
        path: 'projects/$project',
        component: lazyRouteComponent(() => import('./project-page.tsx'), 'ProjectPage'),
      }),
      createRoute({
        getParentRoute: () => projectsLayout,
        path: 'projects/$project/workstreams/$workstream',
        component: lazyRouteComponent(() => import('./workstream-page.tsx'), 'WorkstreamPage'),
      }),
      createRoute({
        getParentRoute: () => projectsLayout,
        path: 'tasks/$task',
        component: lazyRouteComponent(() => import('./task-page.tsx'), 'TaskPage'),
      }),
      createRoute({
        getParentRoute: () => projectsLayout,
        path: 'settings/integrations',
        staticData: { title: 'Integrations' },
        component: lazyRouteComponent(() => import('./integrations/integrations-page.tsx'), 'IntegrationsPage'),
      }),
    ]),
  ];
}

export const feature = defineFeature({
  id: 'projects',
  layout: 'projects',
  routes: projectsRoutes,
  // Entries sort before the shell's own "Agent console" (order 40): e2e/shell.spec.ts's keyboard
  // test tabs from it straight to the projects tree, which comes right after the top nav list.
  nav: [
    { id: 'projects-list', label: 'Projects', to: 'projects', order: 25 },
    { id: 'members', label: 'Members', to: 'members', order: 35 },
    { id: 'calendar', label: 'Calendar', to: 'calendar', order: 37 },
    { id: 'integrations', label: 'Integrations', to: 'settings/integrations', order: 38 },
  ],
  commands: [
    { id: 'go-projects', label: 'Go to Projects', group: 'Go to', run: (c) => c.go('projects') },
    { id: 'go-calendar', label: 'Go to Calendar', group: 'Go to', run: (c) => c.go('calendar') },
    {
      id: 'go-integrations',
      label: 'Go to Integrations',
      group: 'Go to',
      keywords: ['github', 'jira', 'settings', 'sync'],
      run: (c) => c.go('settings/integrations'),
    },
  ],
  create: [{ id: 'task', label: 'Task', order: 10, dialog: lazy(() => import('./new-task.tsx').then((m) => ({ default: m.NewTaskDialog }))) }],
});

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
export const MyTasksPage = lazy(() => import('./my-tasks.tsx').then((m) => ({ default: m.MyTasksPage })));
export const ProjectsListPage = lazy(() => import('./projects-list.tsx').then((m) => ({ default: m.ProjectsListPage })));
export const MembersPage = lazy(() => import('./members.tsx').then((m) => ({ default: m.MembersPage })));
export const ProjectPage = lazy(() => import('./project-page.tsx').then((m) => ({ default: m.ProjectPage })));
export const WorkstreamPage = lazy(() => import('./workstream-page.tsx').then((m) => ({ default: m.WorkstreamPage })));
export const TaskPage = lazy(() => import('./task-page.tsx').then((m) => ({ default: m.TaskPage })));
/** A session's blocks of work under a "Work" heading (`level` 2 or 3), for the console's session page. */
export const SessionWork = lazy(() => import('./recaps.tsx').then((m) => ({ default: m.SessionWork })));
/** The Files tab's viewer and folder tree, shared with the console's workbench (`file-viewer.tsx`). */
export const FileViewer = lazy(() => import('./file-viewer.tsx').then((m) => ({ default: m.FileViewer })));
export const FileTree = lazy(() => import('./file-viewer.tsx').then((m) => ({ default: m.FileTree })));
export type { FileDraft, FileViewerProps } from './file-viewer.tsx';

export { ProjectsNavProvider, useProjectsNav, type ProjectsNav } from './nav.tsx';
/** Open asks to me, for the sidebar's Inbox badge. */
export { useInbox } from './data.ts';
export type { BoardGrouping, BoardProps } from './board.tsx';
