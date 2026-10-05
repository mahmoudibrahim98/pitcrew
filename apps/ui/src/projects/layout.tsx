// Wires `ProjectsNavProvider` to the shell's router: every "open X" in the projects components
// becomes a navigation to the shell's well-known paths (see shell/README.md, "Wiring components
// that already exist"). One pathless route wraps every projects route, so it runs once.

import { Outlet, useRouter } from '@tanstack/react-router';
import { useState } from 'react';
import { TaskDrawer } from './task-drawer.tsx';
import { paths, useWorkspaceId } from '../shell/index.ts';
import { ProjectsNavProvider, type ProjectsNav } from './nav.tsx';

export function ProjectsLayout() {
  const router = useRouter();
  const ws = useWorkspaceId();
  const [taskId, setTaskId] = useState<string | null>(null);
  const nav: ProjectsNav = {
    openTask: setTaskId,
    openTaskPage: (task) => { setTaskId(null); void router.navigate({ href: paths.task(ws, task) }); },
    taskLink: (task) => new URL(paths.task(ws, task), window.location.origin).href,
    openProject: (project) => void router.navigate({ href: paths.project(ws, project) }),
    // By id only: redirects to the project-scoped path once the workstream's project is known.
    openWorkstream: (workstream) => void router.navigate({ href: paths.workstreamById(ws, workstream) }),
    // The console owns chat vs. terminal inside the session page; both views share one path.
    openSession: (session) => void router.navigate({ href: paths.session(ws, session) }),
    openInbox: () => void router.navigate({ href: paths.inbox(ws) }),
  };
  return (
    <ProjectsNavProvider value={nav}>
      <Outlet />
      {taskId !== null && <TaskDrawer taskId={taskId} open onOpenChange={(open) => { if (!open) setTaskId(null); }} />}
    </ProjectsNavProvider>
  );
}
