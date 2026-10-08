// Projects and their workstreams, live: open-task counts and "needs you" counts (open asks to you
// about a task or session there) update as events arrive.

import { Link, useParams } from '@tanstack/react-router';
import {
  useProjects,
  useSessions,
  useTasks,
  useWorkstreams,
  type Workstream,
} from '../data/index.ts';
import { Badge, Button, FolderIcon, Tree, TreeItem } from '../design/index.ts';
import { cx } from '../lib/cx.ts';
import { useRegistry } from './context.ts';
import { askPlaces, countBy, isOpenTask, useMyOpenAsks } from './data.ts';
import { useWorkspaceId } from './layout.ts';
import { paths } from './paths.ts';
import { useShell } from './store.ts';

function Counts({ id, open, needs }: { id: string; open: number; needs: number }) {
  return (
    <span className="ml-auto flex shrink-0 items-center gap-1">
      {needs > 0 && (
        <Badge tone="warn" label={needs === 1 ? 'needs you' : 'need you'}>
          {needs}
        </Badge>
      )}
      <Badge tone="plain" label={open === 1 ? 'open task' : 'open tasks'} data-testid={`open-${id}`}>
        {open}
      </Badge>
    </span>
  );
}

function HealthDot({ workstream }: { workstream: Workstream }) {
  const idea = workstream.status === 'idea';
  const paused = workstream.status === 'paused';
  const colour =
    workstream.health === 'blocked' ? 'bg-risk' : workstream.health === 'at_risk' ? 'bg-warn' : 'bg-ok';
  return (
    <span className="inline-flex size-4 shrink-0 items-center justify-center">
      <span
        aria-hidden
        className={cx(
          'size-2 rounded-pill',
          idea ? 'border border-dashed border-ink-2' : paused ? 'bg-line-2' : colour,
        )}
      />
      {!idea && !paused && workstream.health !== 'on_track' && (
        <span className="sr-only">{workstream.health === 'blocked' ? 'Blocked:' : 'At risk:'}</span>
      )}
    </span>
  );
}

export function ProjectsTree() {
  const ws = useWorkspaceId();
  const params: { project?: string; workstream?: string } = useParams({ strict: false });
  const projects = useProjects().data ?? [];
  const workstreams = useWorkstreams().data ?? [];
  const tasks = useTasks().data ?? [];
  const sessions = useSessions().data ?? [];
  const asks = useMyOpenAsks().data ?? [];
  const expanded = useShell((s) => s.expanded);
  const setExpanded = useShell((s) => s.setExpanded);
  // The Projects feature's "New project" dialog; without it, nothing to offer.
  const canCreate = useRegistry().create.some((e) => e.id === 'project' && e.disabled === undefined);

  const open = tasks.filter(isOpenTask);
  const openByProject = countBy(open, (t) => t.project);
  const openByWorkstream = countBy(open, (t) => t.workstream);
  const places = askPlaces(asks, tasks, sessions, workstreams);
  const needsByProject = countBy(places, (p) => p.project);
  const needsByWorkstream = countBy(places, (p) => p.workstream);

  const visible = projects.filter((p) => p.status !== 'completed');
  if (visible.length === 0) {
    return <div className="m-1 rounded-md border border-dashed border-line p-3 text-sm text-ink-2">
      <FolderIcon className="mb-2" /><p>No projects yet.</p><p className="mt-1 text-xs">Group your tasks and agent sessions in a project.</p>
      {canCreate && <Button className="mt-3" onClick={(event) => useShell.getState().setCreating('project', event.currentTarget)}>New project</Button>}
    </div>;
  }

  return (
    <Tree label="Projects" defaultValue={params.workstream ?? params.project ?? visible[0]?.id}>
      {visible.map((project) => {
        const streams = workstreams.filter(
          (w) => w.project === project.id && w.status !== 'shipped' && w.status !== 'dropped',
        );
        const isOpen = expanded[project.id] ?? params.project === project.id;
        return (
          <TreeItem
            key={project.id}
            value={project.id}
            level={1}
            expanded={streams.length > 0 ? isOpen : undefined}
            onExpandedChange={(next) => setExpanded(project.id, next)}
            current={params.project === project.id && params.workstream === undefined}
            groupLabel={project.name}
            items={streams.map((w) => (
              <TreeItem key={w.id} value={w.id} level={2} current={params.workstream === w.id}>
                <Link to={paths.workstream(ws, project.id, w.id)} activeOptions={{ exact: true }}>
                  <HealthDot workstream={w} />
                  <span className="min-w-0 flex-1 truncate">{w.name}</span>
                  <Counts id={w.id} open={openByWorkstream.get(w.id) ?? 0} needs={needsByWorkstream.get(w.id) ?? 0} />
                </Link>
              </TreeItem>
            ))}
          >
            <Link to={paths.project(ws, project.id)} activeOptions={{ exact: true }}>
              <FolderIcon className="text-ink-2" />
              <span className="min-w-0 flex-1 truncate">{project.name}</span>
              <Counts
                id={project.id}
                open={openByProject.get(project.id) ?? 0}
                needs={needsByProject.get(project.id) ?? 0}
              />
            </Link>
          </TreeItem>
        );
      })}
    </Tree>
  );
}
