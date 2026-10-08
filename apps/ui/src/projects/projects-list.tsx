// The projects list: every project, its status, lead and open task count.

import { StatusPill } from '../design/index.ts';
import { PROJECT_STATUS } from './format.ts';
import { useMemberMap, useProjects, useTasks } from './data.ts';
import { useProjectsNav } from './nav.tsx';
import { ErrorNote, MaybeLink } from './ui.tsx';

export function ProjectsListPage() {
  const projects = useProjects();
  const tasks = useTasks();
  const members = useMemberMap();
  const nav = useProjectsNav();
  const openProject = nav.openProject;
  const rows = [...(projects.data ?? [])].sort((a, b) => a.name.localeCompare(b.name));

  return (
    <div className="mx-auto flex max-w-4xl flex-col gap-4 px-6 py-6">
      <h1 className="text-2xl font-semibold">Projects</h1>
      {projects.error !== null && <ErrorNote error={projects.error} what="load the projects" />}
      {projects.data !== undefined && rows.length === 0 && <p className="text-sm text-ink-2">No projects yet.</p>}
      {rows.length > 0 && (
        <ul aria-label="Projects" className="flex flex-col divide-y divide-line rounded-md border border-line bg-card">
          {rows.map((project) => {
            const lead = members.get(project.lead);
            const open = (tasks.data ?? []).filter(
              (t) => t.project === project.id && t.archived !== true && t.status !== 'done' && t.status !== 'canceled',
            ).length;
            return (
              <li key={project.id} className="flex flex-wrap items-center gap-2 px-4 py-3">
                <span className="w-14 shrink-0 font-mono text-xs text-ink-2">{project.key}</span>
                <MaybeLink
                  onOpen={openProject === undefined ? undefined : () => openProject(project.id)}
                  className="font-medium"
                >
                  {project.name}
                </MaybeLink>
                <StatusPill tone={PROJECT_STATUS[project.status].tone}>
                  {PROJECT_STATUS[project.status].label}
                </StatusPill>
                {lead !== undefined && <span className="text-xs text-ink-2">Lead: {lead.handle}</span>}
                <span className="ml-auto text-xs text-ink-2">
                  {open} open {open === 1 ? 'task' : 'tasks'}
                </span>
              </li>
            );
          })}
        </ul>
      )}
    </div>
  );
}
