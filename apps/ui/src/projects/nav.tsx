// Where the projects components send people. The shell supplies these once it has routes; until
// then a component with no handler renders its target as plain text instead of a dead link.

import { createContext, use, type ReactNode } from 'react';
import type { ProjectId, Receipt, SessionId, TaskId, WorkstreamId } from '../data/index.ts';

export interface ProjectsNav {
  openTask?(task: TaskId): void;
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
