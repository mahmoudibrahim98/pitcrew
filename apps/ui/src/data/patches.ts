// Events that carry the whole object are written straight into the cache instead of refetching.

import type { QueryClient } from '@tanstack/react-query';
import { keys } from './keys.ts';
import type { EventBody, EventType, Session, SessionFilters, Task, TaskFilters } from './types.ts';

type DataOf<T extends EventType> = Extract<EventBody, { type: T }>['data'];

export type PatchMap = { [T in EventType]?: (data: DataOf<T>, queryClient: QueryClient) => void };

export function taskMatches(filters: TaskFilters, task: Task): boolean {
  return (
    (filters.project === undefined || filters.project === task.project) &&
    (filters.workstream === undefined || filters.workstream === task.workstream) &&
    (filters.assignee === undefined || filters.assignee === task.assignee) &&
    (filters.status === undefined || filters.status.length === 0 || filters.status.includes(task.status))
  );
}

export function sessionMatches(filters: SessionFilters, session: Session): boolean {
  return (
    (filters.machine === undefined || filters.machine === session.machine) &&
    (filters.workstream === undefined || filters.workstream === session.workstream) &&
    (filters.task === undefined || filters.task === session.task) &&
    (filters.state === undefined || filters.state === session.state)
  );
}

function upsert<T extends { id: string }>(list: T[], item: T): T[] {
  const index = list.findIndex((x) => x.id === item.id);
  return index === -1 ? [...list, item] : list.map((x, i) => (i === index ? item : x));
}

/** Updates every cached list under `prefix`; the filters are the key's third element. */
function updateLists<T, F>(
  queryClient: QueryClient,
  prefix: readonly unknown[],
  update: (list: T[], filters: F) => T[],
): void {
  for (const [key, list] of queryClient.getQueriesData<T[]>({ queryKey: prefix })) {
    if (list !== undefined) queryClient.setQueryData(key, update(list, (key[2] ?? {}) as F));
  }
}

export const patches: PatchMap = {
  task_created: ({ task }, qc) => {
    qc.setQueryData(keys.tasks.detail(task.id), task);
    updateLists<Task, TaskFilters>(qc, keys.tasks.lists, (list, filters) =>
      taskMatches(filters, task) ? upsert(list, task) : list,
    );
  },
  subtasks_replaced: ({ task: id, subtasks }, qc) => {
    const replace = (task: Task): Task => (task.id === id ? { ...task, subtasks } : task);
    const detail = qc.getQueryData<Task>(keys.tasks.detail(id));
    if (detail !== undefined) qc.setQueryData(keys.tasks.detail(id), replace(detail));
    updateLists<Task, TaskFilters>(qc, keys.tasks.lists, (list) =>
      list.some((t) => t.id === id) ? list.map(replace) : list,
    );
  },
  session_discovered: ({ session }, qc) => {
    qc.setQueryData(keys.sessions.detail(session.id), session);
    updateLists<Session, SessionFilters>(qc, keys.sessions.lists, (list, filters) =>
      sessionMatches(filters, session) ? upsert(list, session) : list,
    );
  },
};

export function applyPatches(queryClient: QueryClient, events: readonly { body: EventBody }[]): void {
  for (const { body } of events) {
    if (!Object.hasOwn(patches, body.type)) continue;
    const patch = patches[body.type] as ((data: unknown, qc: QueryClient) => void) | undefined;
    patch?.(body.data, queryClient);
  }
}
