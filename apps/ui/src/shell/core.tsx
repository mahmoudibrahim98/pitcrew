// The shell's own feature: Home, Inbox, My tasks and the Agent console in the sidebar, the
// placeholder pages the Projects and Agent console features replace, the shell's palette
// commands, and placeholder "+ New" dialogs.

import { createRoute, lazyRouteComponent, type AnyRoute } from '@tanstack/react-router';
import { useMe, useSessions, useTasks } from '../data/index.ts';
import {
  Badge,
  Button,
  CheckCircleIcon,
  ConsoleIcon,
  DialogFooter,
  HomeIcon,
  InboxIcon,
  useTheme,
} from '../design/index.ts';
import { isOpenTask, useMyOpenAsks } from './data.ts';
import type { CreateEntry, Feature } from './feature.ts';
import { OpenWorkstream } from './pages/open.tsx';
import type { WorkspaceRoute } from './routes.tsx';
import { useShell } from './store.ts';

function InboxCount() {
  const n = useMyOpenAsks().data?.length ?? 0;
  if (n === 0) return null;
  return (
    <Badge tone="accent" label={n === 1 ? 'open ask' : 'open asks'} data-testid="count-inbox">
      {n}
    </Badge>
  );
}

function MyTasksCount() {
  const me = useMe().data?.id;
  const tasks = useTasks().data;
  if (me === undefined || tasks === undefined) return null;
  const n = tasks.filter((t) => t.assignee === me && isOpenTask(t)).length;
  return (
    <Badge tone="plain" label={n === 1 ? 'open task' : 'open tasks'} data-testid="count-my-tasks">
      {n}
    </Badge>
  );
}

function ConsoleCount() {
  const sessions = useSessions().data;
  if (sessions === undefined) return null;
  const working = sessions.filter((s) => s.state === 'working').length;
  const waiting = sessions.filter((s) => s.state === 'waiting').length;
  return (
    <span className="flex items-center gap-1" data-testid="count-console">
      <Badge tone="neutral" label="working" className="gap-1">
        <span aria-hidden className="size-1.5 rounded-pill bg-ok" />
        {working}
      </Badge>
      {waiting > 0 && (
        <Badge tone="warn" label="waiting">
          {waiting}
        </Badge>
      )}
    </span>
  );
}

function placeholder(what: string) {
  function PlaceholderDialog({ close }: { close(): void }) {
    return (
      <>
        <p className="px-4 py-4 text-sm text-ink-2">
          Creating {what} is not available yet. This dialog is a placeholder.
        </p>
        <DialogFooter>
          <Button variant="primary" onClick={close}>
            Close
          </Button>
        </DialogFooter>
      </>
    );
  }
  return PlaceholderDialog;
}

const CREATE: CreateEntry[] = [
  { id: 'task', label: 'Task', order: 10, dialog: placeholder('a task') },
  { id: 'agent', label: 'Agent', order: 20, dialog: placeholder('an agent') },
  { id: 'project', label: 'Project', order: 30, dialog: placeholder('a project') },
  { id: 'team', label: 'Team', order: 40, dialog: placeholder('a team') },
];

type PageName = keyof typeof import('./pages/placeholders.tsx');

const pages = (name: PageName) => lazyRouteComponent(() => import('./pages/placeholders.tsx'), name);

/** Placeholder pages at the well-known paths. A feature route at the same path replaces one. */
function placeholderRoutes(parent: WorkspaceRoute): AnyRoute[] {
  const route = (
    path: string,
    component: PageName,
    staticData: { layout: Feature['layout']; title?: string; setup?: boolean },
  ) => createRoute({ getParentRoute: () => parent, path, component: pages(component), staticData });
  return [
    createRoute({ getParentRoute: () => parent, path: 'settings', component: lazyRouteComponent(() => import('./updates.tsx'), 'UpdateSettings'), staticData: { layout: 'both', title: 'Settings' } }),
    route('home', 'HomePage', { layout: 'projects', title: 'Home' }),
    route('inbox', 'InboxPage', { layout: 'both', title: 'Inbox' }),
    route('my-tasks', 'MyTasksPage', { layout: 'projects', title: 'My tasks' }),
    route('projects/$project', 'ProjectPage', { layout: 'projects' }),
    route('projects/$project/workstreams/$workstream', 'WorkstreamPage', { layout: 'projects' }),
    createRoute({
      getParentRoute: () => parent,
      path: 'workstreams/$workstream',
      component: OpenWorkstream,
      staticData: { layout: 'projects' },
    }),
    route('tasks/$task', 'TaskPage', { layout: 'projects' }),
    route('console', 'ConsolePage', { layout: 'console', title: 'Agent console' }),
    route('console/$session', 'SessionPage', { layout: 'console' }),
    // `paths.setup`: where a workspace with `setup_needed` is sent, so it exists in every build.
    route('onboarding', 'SetupPage', { layout: 'both', title: 'Set up', setup: true }),
  ];
}

export const shellFeature: Feature = {
  id: 'shell',
  layout: 'both',
  routes: placeholderRoutes,
  nav: [
    { id: 'files', label: 'Files', to: 'files', order: 45 },
    { id: 'home', label: 'Home', to: 'home', icon: HomeIcon, layout: 'projects', order: 10 },
    { id: 'inbox', label: 'Inbox', to: 'inbox', icon: InboxIcon, badge: InboxCount, order: 20 },
    {
      id: 'my-tasks',
      label: 'My tasks',
      to: 'my-tasks',
      icon: CheckCircleIcon,
      badge: MyTasksCount,
      layout: 'projects',
      order: 30,
    },
    { id: 'console', label: 'Agent console', to: 'console', icon: ConsoleIcon, badge: ConsoleCount, order: 40 },
  ],
  commands: [
    { id: 'quick-open', label: 'Quick open a file…', group: 'Files', keys: ['mod', 'p'], run: c => { if (document.querySelector('[aria-label="File explorer"]')) window.dispatchEvent(new Event('pitcrew:quick-open')); else c.go('files?quick=1'); } },
    { id: 'go-home', label: 'Go to Home', group: 'Go to', layout: 'projects', run: (c) => c.go('home') },
    { id: 'go-inbox', label: 'Go to Inbox', group: 'Go to', run: (c) => c.go('inbox') },
    { id: 'go-my-tasks', label: 'Go to My tasks', group: 'Go to', layout: 'projects', run: (c) => c.go('my-tasks') },
    {
      id: 'layout-console',
      label: 'Switch to the Agent console',
      keywords: ['layout', 'sessions', 'agents'],
      keys: ['mod', '.'],
      layout: 'projects',
      run: (c) => c.switchLayout('console'),
    },
    {
      id: 'layout-projects',
      label: 'Switch to Projects',
      keywords: ['layout'],
      keys: ['mod', '.'],
      layout: 'console',
      run: (c) => c.switchLayout('projects'),
    },
    {
      id: 'toggle-orchestrator',
      label: 'Show or hide the Orchestrator',
      keywords: ['panel', 'assistant'],
      keys: ['mod', 'j'],
      run: () => {
        const shell = useShell.getState();
        shell.setOrchestratorOpen(!shell.orchestratorOpen);
      },
    },
    {
      id: 'toggle-sidebar',
      label: 'Collapse or expand the sidebar',
      keys: ['mod', 'b'],
      run: () => {
        const shell = useShell.getState();
        shell.setSidebarCollapsed(!shell.sidebarCollapsed);
      },
    },
    { id: 'theme-light', label: 'Use the light theme', group: 'Theme', run: () => useTheme.getState().setTheme('light') },
    { id: 'theme-dark', label: 'Use the dark theme', group: 'Theme', run: () => useTheme.getState().setTheme('dark') },
    {
      id: 'theme-system',
      label: 'Follow the system theme',
      group: 'Theme',
      run: () => useTheme.getState().setTheme('system'),
    },
  ],
  create: CREATE,
};
