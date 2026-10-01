// Home: where things stand across projects, the agents at work, what needs you, and what changed
// since you last looked (the last revision you marked as seen, kept in local storage for now).

import { create } from 'zustand';
import { persist } from 'zustand/middleware';
import { Button, StatusPill } from '../design/index.ts';
import { useWorkspace } from '../data/index.ts';
import { AuthorAvatar } from './activity.tsx';
import { AgentsNow } from './agents.tsx';
import {
  sameTarget,
  useActivity,
  useBriefs,
  useInbox,
  useMemberMap,
  useNames,
  useProjects,
  useTasks,
  useWorkstreams,
  withRevisions,
} from './data.ts';
import { ASK_KIND, PROJECT_STATUS, describeEvent, formatWhen } from './format.ts';
import { useProjectsNav } from './nav.tsx';
import { ErrorNote, MaybeLink, Panel } from './ui.tsx';

/** The last revision marked as seen, per workspace. UI state only; it never reaches the hub. */
export const useLastSeen = create<{
  byWorkspace: Record<string, number>;
  markSeen(workspace: string, rev: number): void;
}>()(
  persist(
    (set) => ({
      byWorkspace: {},
      markSeen: (workspace, rev) => set((s) => ({ byWorkspace: { ...s.byWorkspace, [workspace]: rev } })),
    }),
    { name: 'pitcrew.projects.last-seen' },
  ),
);

export function Home() {
  return (
    <div className="flex flex-col gap-4">
      <h1 className="text-2xl font-semibold">Home</h1>
      <div className="grid gap-4 lg:grid-cols-[minmax(0,2fr)_minmax(0,1fr)]">
        <div className="flex min-w-0 flex-col gap-4">
          <WhereThingsStand />
          <SinceLastLooked />
        </div>
        <div className="flex min-w-0 flex-col gap-4">
          <NeedsYou />
          <AgentsNow />
        </div>
      </div>
    </div>
  );
}

function WhereThingsStand() {
  const projects = useProjects();
  const briefs = useBriefs();
  const tasks = useTasks();
  const workstreams = useWorkstreams();
  const nav = useProjectsNav();
  const openProject = nav.openProject;
  return (
    <Panel title="Where things stand">
      {projects.error !== null && <ErrorNote error={projects.error} what="load the projects" />}
      <ul aria-label="Projects" className="flex flex-col divide-y divide-line">
        {(projects.data ?? []).map((project) => {
          const brief = briefs.data?.find((b) => sameTarget(b.target, { kind: 'project', id: project.id }));
          const open = (tasks.data ?? []).filter(
            (t) => t.project === project.id && t.status !== 'done' && t.status !== 'canceled',
          ).length;
          const atRisk = (workstreams.data ?? []).filter(
            (w) => w.project === project.id && w.health !== 'on_track' && w.status === 'active',
          ).length;
          return (
            <li key={project.id} className="flex flex-col gap-1 py-3 first:pt-0 last:pb-0">
              <div className="flex flex-wrap items-center gap-2">
                <MaybeLink
                  onOpen={openProject === undefined ? undefined : () => openProject(project.id)}
                  className="text-md font-semibold"
                >
                  {project.name}
                </MaybeLink>
                <StatusPill tone={PROJECT_STATUS[project.status].tone}>{PROJECT_STATUS[project.status].label}</StatusPill>
                <span className="ml-auto text-xs text-ink-2">
                  {open} open {open === 1 ? 'task' : 'tasks'}
                  {atRisk > 0 && ` · ${atRisk} ${atRisk === 1 ? 'workstream' : 'workstreams'} at risk or blocked`}
                </span>
              </div>
              <p className="text-sm">{brief?.text ?? 'Nothing written yet.'}</p>
              {brief?.next !== undefined && (
                <p className="text-sm text-ink-2">
                  <span className="font-medium text-ink">Next: </span>
                  {brief.next}
                </p>
              )}
            </li>
          );
        })}
      </ul>
    </Panel>
  );
}

function NeedsYou() {
  const inbox = useInbox();
  const names = useNames();
  const nav = useProjectsNav();
  const asks = [...(inbox.data ?? [])].sort((a, b) => b.created - a.created);
  const openInbox = nav.openInbox;
  return (
    <Panel
      title="Needs you"
      actions={
        openInbox !== undefined && asks.length > 0 ? (
          <Button variant="ghost" onClick={openInbox}>
            Open the Inbox
          </Button>
        ) : undefined
      }
    >
      {inbox.error !== null && <ErrorNote error={inbox.error} what="load what needs you" />}
      {inbox.data !== undefined && (
        <p className="mb-2 text-sm text-ink-2">
          {asks.length === 0 ? 'Nothing needs you right now.' : `${asks.length} open ${asks.length === 1 ? 'ask' : 'asks'}`}
        </p>
      )}
      {asks.length > 0 && (
        <ul aria-label="Open asks" className="flex flex-col gap-2">
          {asks.slice(0, 5).map((ask) => (
            <li key={ask.id} className="flex flex-col text-sm">
              <span className="text-xs text-ink-2">
                {ASK_KIND[ask.kind].one}
                {ask.task === undefined ? '' : ` · ${names.task(ask.task)}`}
              </span>
              <span>{ask.title}</span>
            </li>
          ))}
        </ul>
      )}
    </Panel>
  );
}

function SinceLastLooked() {
  const workspace = useWorkspace();
  const id = workspace.data?.workspace.id;
  const lastSeen = useLastSeen((s) => (id === undefined ? undefined : s.byWorkspace[id]));
  const markSeen = useLastSeen((s) => s.markSeen);
  const activity = useActivity();
  const members = useMemberMap();
  const names = useNames();
  const page = activity.data;
  const all = page === undefined ? [] : withRevisions(page);
  const fresh = (lastSeen === undefined ? all.slice(-10) : all.filter((e) => e.rev > lastSeen)).reverse();
  const more = page !== undefined && lastSeen !== undefined && !page.at_start && page.from_rev > lastSeen + 1;
  return (
    <Panel
      title="Since you last looked"
      actions={
        id !== undefined && page !== undefined && fresh.length > 0 ? (
          <Button variant="ghost" onClick={() => markSeen(id, page.to_rev)}>
            Mark all as seen
          </Button>
        ) : undefined
      }
    >
      {activity.error !== null && <ErrorNote error={activity.error} what="load what changed" />}
      {page !== undefined && lastSeen === undefined && (
        <p className="mb-2 text-sm text-ink-2">The latest changes. Mark them as seen to see only what is new next time.</p>
      )}
      {page !== undefined && lastSeen !== undefined && fresh.length === 0 && (
        <p className="text-sm text-ink-2">Nothing new since you last looked.</p>
      )}
      {fresh.length > 0 && (
        <ol aria-label="Changes" className="flex flex-col gap-1.5">
          {fresh.map(({ event }) => (
            <li key={event.id} className="flex items-start gap-2 text-sm">
              <AuthorAvatar event={event} members={members} />
              <p className="min-w-0 flex-1">
                <span className="font-medium">{names.member(event.author)}</span>{' '}
                <span className="text-ink-2">{describeEvent(event, names)}</span>
              </p>
              <time dateTime={new Date(event.at).toISOString()} className="shrink-0 text-xs text-ink-2">
                {formatWhen(event.at)}
              </time>
            </li>
          ))}
        </ol>
      )}
      {more && <p className="mt-2 text-xs text-ink-2">Showing the latest {all.length}; more happened before.</p>}
    </Panel>
  );
}
