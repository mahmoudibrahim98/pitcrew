// A flat, sorted list of a workstream's tasks (the "Tasks" tab): not a board, just the facts.

import { StatusPill } from '../design/index.ts';
import type { WorkstreamId } from '../data/index.ts';
import { useMemberMap, useTasks } from './data.ts';
import { PRIORITY, TASK_STATUS, formatDay, STATUS_ORDER } from './format.ts';
import { useProjectsNav } from './nav.tsx';
import { MemberChip } from './people.tsx';
import { ErrorNote, MaybeLink } from './ui.tsx';
import { CardList } from './virtual-list.tsx';

function rank(status: (typeof STATUS_ORDER)[number]): number {
  const i = STATUS_ORDER.indexOf(status);
  return i === -1 ? STATUS_ORDER.length : i;
}

export function TasksList({ workstream }: { workstream: WorkstreamId }) {
  const tasks = useTasks({ workstream });
  const members = useMemberMap();
  const nav = useProjectsNav();
  const openTask = nav.openTask;
  const list = [...(tasks.data ?? []).filter((task) => !task.archived)].sort(
    (a, b) => rank(a.status) - rank(b.status) || PRIORITY[a.priority].rank - PRIORITY[b.priority].rank,
  );

  return (
    <div className="flex flex-col gap-2">
      {tasks.error !== null && <ErrorNote error={tasks.error} what="load the tasks" />}
      {tasks.data !== undefined && list.length === 0 && <p className="text-sm text-ink-2">No tasks yet.</p>}
      {list.length > 0 && (
        <CardList
          items={list}
          itemKey={(t) => t.id}
          label="Tasks"
          renderItem={(task) => {
            const assignee = task.assignee === undefined ? undefined : members.get(task.assignee);
            const owner = assignee?.owner === undefined ? undefined : members.get(assignee.owner);
            return (
              <div className="flex flex-wrap items-center gap-2 rounded-md border border-line bg-card p-2.5 text-sm">
                <span className="font-mono text-xs text-ink-2">{task.key}</span>
                <MaybeLink onOpen={openTask === undefined ? undefined : () => openTask(task.id)} className="font-medium">
                  {task.title}
                </MaybeLink>
                <StatusPill tone={TASK_STATUS[task.status].tone}>{TASK_STATUS[task.status].label}</StatusPill>
                {task.priority !== 'none' && (
                  <span className="text-xs text-ink-2">{PRIORITY[task.priority].label}</span>
                )}
                <span className="ml-auto flex items-center gap-2">
                  {task.due !== undefined && <span className="text-xs text-ink-2">Due {formatDay(task.due)}</span>}
                  {assignee !== undefined ? <MemberChip member={assignee} owner={owner} /> : (
                    <span className="text-xs text-ink-2">Unassigned</span>
                  )}
                </span>
              </div>
            );
          }}
        />
      )}
    </div>
  );
}
