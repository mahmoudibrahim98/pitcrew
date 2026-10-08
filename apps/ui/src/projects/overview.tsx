// Project and workstream overviews: where it stands, the workstreams, what needs you, the agents on
// it, and recent activity.

import { StatusPill } from '../design/index.ts';
import type { ProjectId, Task, TaskStatus, WorkstreamId } from '../data/index.ts';
import { ActivityFeed } from './activity.tsx';
import { AgentsNow } from './agents.tsx';
import {
  sameTarget,
  useBriefs,
  useInbox,
  useNames,
  useOptionalWorkstream,
  useProject,
  useTasks,
  useWorkstreams,
} from './data.ts';
import { HEALTH, PROJECT_STATUS, STATUS_ORDER, TASK_STATUS, WORKSTREAM_STATUS, formatDay } from './format.ts';
import { useProjectsNav } from './nav.tsx';
import { ErrorNote, MaybeLink, Panel } from './ui.tsx';
import { WhereItStands } from './where-it-stands.tsx';

const OPEN: readonly TaskStatus[] = ['backlog', 'todo', 'in_progress', 'review'];
const isOpen = (task: Task) => task.archived !== true && OPEN.includes(task.status);

/** Workstreams with status, health, the next step from their brief, and open tasks. */
export function WorkstreamsTable({ project }: { project: ProjectId }) {
  const workstreams = useWorkstreams(project);
  const briefs = useBriefs();
  const tasks = useTasks({ project });
  const nav = useProjectsNav();
  const openWorkstream = nav.openWorkstream;
  const rows = workstreams.data ?? [];
  return (
    <Panel title="Workstreams">
      {workstreams.error !== null && <ErrorNote error={workstreams.error} what="load the workstreams" />}
      {workstreams.data !== undefined && rows.length === 0 && <p className="text-sm text-ink-2">No workstreams yet.</p>}
      {rows.length > 0 && (
        <div className="overflow-x-auto">
          <table className="w-full text-sm">
            <caption className="sr-only">Workstreams</caption>
            <thead>
              <tr className="border-b border-line text-left text-xs text-ink-2">
                <th scope="col" className="py-1.5 pr-3 font-medium">
                  Workstream
                </th>
                <th scope="col" className="py-1.5 pr-3 font-medium">
                  Status
                </th>
                <th scope="col" className="py-1.5 pr-3 font-medium">
                  Health
                </th>
                <th scope="col" className="py-1.5 pr-3 font-medium">
                  Next
                </th>
                <th scope="col" className="py-1.5 text-right font-medium">
                  Open tasks
                </th>
              </tr>
            </thead>
            <tbody>
              {rows.map((w) => {
                const brief = briefs.data?.find((b) => sameTarget(b.target, { kind: 'workstream', id: w.id }));
                const open = (tasks.data ?? []).filter((t) => t.workstream === w.id && isOpen(t)).length;
                return (
                  <tr key={w.id} className="border-b border-line last:border-0">
                    <th scope="row" className="py-2 pr-3 text-left font-medium">
                      <MaybeLink onOpen={openWorkstream === undefined ? undefined : () => openWorkstream(w.id)}>
                        {w.name}
                      </MaybeLink>
                    </th>
                    <td className="py-2 pr-3">
                      <StatusPill tone={WORKSTREAM_STATUS[w.status].tone}>{WORKSTREAM_STATUS[w.status].label}</StatusPill>
                    </td>
                    <td className="py-2 pr-3">
                      <StatusPill tone={HEALTH[w.health].tone}>{HEALTH[w.health].label}</StatusPill>
                    </td>
                    <td className="py-2 pr-3 text-ink-2">{brief?.next ?? '—'}</td>
                    <td className="py-2 text-right tabular-nums">{open}</td>
                  </tr>
                );
              })}
            </tbody>
          </table>
        </div>
      )}
    </Panel>
  );
}

/** Open asks to me about tasks in a project or workstream. */
export function NeedsYouPanel({ project, workstream }: { project?: ProjectId; workstream?: WorkstreamId }) {
  const inbox = useInbox();
  const tasks = useTasks(workstream !== undefined ? { workstream } : project !== undefined ? { project } : {});
  const names = useNames();
  const nav = useProjectsNav();
  const here = new Set((tasks.data ?? []).map((t) => t.id));
  const asks = (inbox.data ?? []).filter((a) => a.task !== undefined && here.has(a.task));
  const openInbox = nav.openInbox;
  const openTask = nav.openTask;
  return (
    <Panel title="Needs you">
      {inbox.data !== undefined && asks.length === 0 && <p className="text-sm text-ink-2">Nothing needs you here.</p>}
      {asks.length > 0 && (
        <ul aria-label="Needs you" className="flex flex-col gap-2">
          {asks.map((ask) => {
            const task = ask.task;
            return (
              <li key={ask.id} className="flex flex-col gap-0.5 text-sm">
                <span className="font-medium">{ask.title}</span>
                {task !== undefined && (
                  <MaybeLink
                    onOpen={openTask === undefined ? undefined : () => openTask(task)}
                    className="font-mono text-xs text-ink-2"
                  >
                    {names.task(task)}
                  </MaybeLink>
                )}
              </li>
            );
          })}
        </ul>
      )}
      {openInbox !== undefined && asks.length > 0 && (
        <MaybeLink onOpen={openInbox} className="mt-2 text-sm text-accent-text">
          Answer in the Inbox
        </MaybeLink>
      )}
    </Panel>
  );
}

/** "Where the project stands", its workstreams, what needs you, agents and activity — no header. */
export function ProjectOverviewBody({ project }: { project: ProjectId }) {
  return (
    <div className="grid gap-4 lg:grid-cols-[minmax(0,2fr)_minmax(0,1fr)]">
      <div className="flex min-w-0 flex-col gap-4">
        <WhereItStands target={{ kind: 'project', id: project }} title="Where the project stands" />
        <WorkstreamsTable project={project} />
        <ActivityFeed filters={{ project }} />
      </div>
      <div className="flex min-w-0 flex-col gap-4">
        <NeedsYouPanel project={project} />
        <AgentsNow project={project} title="Agents on it" />
      </div>
    </div>
  );
}

export function ProjectOverview({ project }: { project: ProjectId }) {
  const data = useProject(project);
  const p = data.data;
  return (
    <div className="flex flex-col gap-4">
      <header className="flex flex-col gap-1">
        {data.error !== null && <ErrorNote error={data.error} what="load the project" />}
        <p className="font-mono text-xs text-ink-2">{p?.key ?? ''}</p>
        <div className="flex flex-wrap items-center gap-2">
          <h1 className="text-2xl font-semibold">{p?.name ?? 'Project'}</h1>
          {p !== undefined && <StatusPill tone={PROJECT_STATUS[p.status].tone}>{PROJECT_STATUS[p.status].label}</StatusPill>}
          {p?.due !== undefined && <span className="text-sm text-ink-2">Due {formatDay(p.due)}</span>}
        </div>
      </header>
      <ProjectOverviewBody project={project} />
    </div>
  );
}

/** "Where it stands", task counts, what needs you, agents and activity — no header. */
export function WorkstreamOverviewBody({ workstream }: { workstream: WorkstreamId }) {
  const tasks = useTasks({ workstream });
  const counts = STATUS_ORDER.map((status) => ({
    status,
    count: (tasks.data ?? []).filter((t) => t.archived !== true && t.status === status).length,
  })).filter((c) => c.count > 0 || c.status !== 'canceled');
  return (
    <div className="grid gap-4 lg:grid-cols-[minmax(0,2fr)_minmax(0,1fr)]">
      <div className="flex min-w-0 flex-col gap-4">
        <WhereItStands target={{ kind: 'workstream', id: workstream }} />
        <Panel title="Tasks">
          <ul aria-label="Tasks by status" className="flex flex-wrap gap-2">
            {counts.map(({ status, count }) => (
              <li key={status}>
                <StatusPill tone={TASK_STATUS[status].tone}>
                  {TASK_STATUS[status].label} {count}
                </StatusPill>
              </li>
            ))}
          </ul>
        </Panel>
        <ActivityFeed filters={{ workstream }} />
      </div>
      <div className="flex min-w-0 flex-col gap-4">
        <NeedsYouPanel workstream={workstream} />
        <AgentsNow workstream={workstream} title="Agents on it" />
      </div>
    </div>
  );
}

export function WorkstreamOverview({ workstream }: { workstream: WorkstreamId }) {
  const data = useOptionalWorkstream(workstream);
  const names = useNames();
  const nav = useProjectsNav();
  const w = data.data;
  const openProject = nav.openProject;
  return (
    <div className="flex flex-col gap-4">
      <header className="flex flex-col gap-1">
        {data.error !== null && <ErrorNote error={data.error} what="load the workstream" />}
        {w !== undefined && (
          <MaybeLink
            onOpen={openProject === undefined ? undefined : () => openProject(w.project)}
            className="text-xs text-ink-2"
          >
            {names.project(w.project)}
          </MaybeLink>
        )}
        <div className="flex flex-wrap items-center gap-2">
          <h1 className="text-2xl font-semibold">{w?.name ?? 'Workstream'}</h1>
          {w !== undefined && (
            <>
              <StatusPill tone={WORKSTREAM_STATUS[w.status].tone}>{WORKSTREAM_STATUS[w.status].label}</StatusPill>
              <StatusPill tone={HEALTH[w.health].tone}>{HEALTH[w.health].label}</StatusPill>
            </>
          )}
        </div>
      </header>
      <WorkstreamOverviewBody workstream={workstream} />
    </div>
  );
}
