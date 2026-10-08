// Where the projects components send people. The shell supplies these once it has routes; until
// then a component with no handler renders its target as plain text instead of a dead link.

import { createContext, use, type ReactNode } from 'react';
import { isDesktop, type ProjectId, type Receipt, type SessionId, type TaskId, type WorkstreamId } from '../data/index.ts';

export interface ProjectsNav {
  openTask?(task: TaskId): void;
  openTaskPage?(task: TaskId): void;
  /** The link "Copy link" copies (see `shareTaskLink`). */
  taskLink?(task: TaskId): string;
  openProject?(project: ProjectId): void;
  openWorkstream?(workstream: WorkstreamId): void;
  /** The console's chat or terminal for a session (stream M). */
  openSession?(session: SessionId, view: 'chat' | 'terminal'): void;
  openInbox?(): void;
  /** A transcript offset, file, job or event behind a claim. */
  openReceipt?(receipt: Receipt): void;
}

const NavContext = createContext<ProjectsNav>({});

export function ProjectsNavProvider({ value, children }: { value: ProjectsNav; children: ReactNode }) {
  return <NavContext value={value}>{children}</NavContext>;
}

export function useProjectsNav(): ProjectsNav {
  return use(NavContext);
}

/** The workspace a path is in (`/w/<ws>/…`), if any. */
export function workspaceIn(pathname: string): string | undefined {
  const ws = /^\/w\/([^/]+)/.exec(pathname)?.[1];
  return ws === undefined ? undefined : decodeURIComponent(ws);
}

/** The task's full page in this window: for navigating, not for sharing. */
export function taskLink(task: string, ws: string | undefined = workspaceIn(window.location.pathname)): string {
  return `${window.location.origin}${ws === undefined ? '' : `/w/${encodeURIComponent(ws)}`}/tasks/${encodeURIComponent(task)}`;
}

/**
 * The link to copy for a task. The desktop app's own address (`tauri://localhost/…`) opens
 * nothing anywhere else, so there it is the `pitcrew://w/<ws>/task/<id>` deep link the app
 * registers; in a browser it is the task page's URL.
 */
export function shareTaskLink(task: string, ws: string | undefined = workspaceIn(window.location.pathname)): string {
  if (isDesktop() && ws !== undefined) return `pitcrew://w/${ws}/task/${task}`;
  return taskLink(task, ws);
}
