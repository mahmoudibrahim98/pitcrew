// Where a session belongs (project and workstream) and the session list's facet filters. Small
// and React-free, so `index.ts` can export it without loading the console.

import type { Engine, Project, Session, SessionState, Task, Workstream } from '../data/index.ts';

/** The facet filters of the session list. An empty list means "any". */
export interface SessionFacets {
  machine: string[];
  engine: Engine[];
  state: SessionState[];
  /** Project ids, or `UNSORTED` for sessions in no project. */
  project: string[];
  workstream: string[];
}

export const UNSORTED = 'unsorted';

export const NO_FACETS: SessionFacets = { machine: [], engine: [], state: [], project: [], workstream: [] };

export function hasFacets(facets: SessionFacets): boolean {
  return Object.values(facets).some((values: readonly string[]) => values.length > 0);
}

/** What grouping and filtering need to know about where sessions belong. */
export interface SessionPlaces {
  projects: readonly Project[];
  workstreams: readonly Workstream[];
  tasks: readonly Task[];
}

/** The project and workstream a session belongs to: its workstream's, else its task's. */
export function placeOf(
  session: Session,
  places: SessionPlaces,
): { project: Project | undefined; workstream: Workstream | undefined } {
  const task = session.task === undefined ? undefined : places.tasks.find((t) => t.id === session.task);
  const workstreamId = session.workstream ?? task?.workstream;
  const workstream =
    workstreamId === undefined ? undefined : places.workstreams.find((w) => w.id === workstreamId);
  const projectId = workstream?.project ?? task?.project;
  const project = projectId === undefined ? undefined : places.projects.find((p) => p.id === projectId);
  return { project, workstream };
}

export function matchesFacets(session: Session, facets: SessionFacets, places: SessionPlaces): boolean {
  const any = <T>(values: readonly T[], value: T) => values.length === 0 || values.includes(value);
  if (!any(facets.machine, session.machine) || !any(facets.engine, session.engine) || !any(facets.state, session.state)) {
    return false;
  }
  if (facets.project.length === 0 && facets.workstream.length === 0) return true;
  const { project, workstream } = placeOf(session, places);
  return (
    any(facets.project, project?.id ?? UNSORTED) &&
    (facets.workstream.length === 0 || (workstream !== undefined && facets.workstream.includes(workstream.id)))
  );
}
