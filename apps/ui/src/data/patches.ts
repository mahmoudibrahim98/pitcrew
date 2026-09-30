// Events that carry the whole object are written straight into the cache instead of refetching.
//
// List data under `['tasks', 'list', filters]` and `['sessions', 'list', filters]` must be plain
// arrays of the wire objects; the filters are the key's third element.

import type { QueryClient } from '@tanstack/react-query';
import type { QueryKey } from './invalidation.ts';
import { keys } from './keys.ts';
import type { EventBody, EventType, Session, SessionFilters, Task, TaskFilters } from './types.ts';

type DataOf<T extends EventType> = Extract<EventBody, { type: T }>['data'];

interface Patch<T extends EventType> {
  /** The keys the patch writes under; they are refetched instead if it fails or races a fetch. */
  keys(data: DataOf<T>): QueryKey[];
  apply(data: DataOf<T>, queryClient: QueryClient): void;
}

export type PatchMap = { [T in EventType]?: Patch<T> };

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

/** Puts `item` in the list if it belongs there, and takes it out if it no longer does. */
function place<T extends { id: string }>(list: T[], item: T, belongs: boolean): T[] {
  const index = list.findIndex((x) => x.id === item.id);
  if (!belongs) return index === -1 ? list : list.filter((_, i) => i !== index);
  return index === -1 ? [...list, item] : list.map((x, i) => (i === index ? item : x));
}

/** Updates every cached list under `prefix`. Throws on data that is not an array. */
function updateLists<T, F>(
  queryClient: QueryClient,
  prefix: QueryKey,
  update: (list: T[], filters: F) => T[],
): void {
  for (const [key, list] of queryClient.getQueriesData<unknown>({ queryKey: prefix })) {
    if (list === undefined) continue;
    if (!Array.isArray(list)) throw new Error(`${JSON.stringify(key)} does not hold a list`);
    queryClient.setQueryData(key, update(list as T[], (key[2] ?? {}) as F));
  }
}

export const patches: PatchMap = {
  task_created: {
    keys: ({ task }) => [keys.tasks.detail(task.id), keys.tasks.lists],
    apply: ({ task }, qc) => {
      qc.setQueryData(keys.tasks.detail(task.id), task);
      updateLists<Task, TaskFilters>(qc, keys.tasks.lists, (list, filters) =>
        place(list, task, taskMatches(filters, task)),
      );
    },
  },
  subtasks_replaced: {
    keys: ({ task }) => [keys.tasks.detail(task), keys.tasks.lists],
    apply: ({ task: id, subtasks }, qc) => {
      const replace = (task: Task): Task => (task.id === id ? { ...task, subtasks } : task);
      const detail = qc.getQueryData<Task>(keys.tasks.detail(id));
      if (detail !== undefined) qc.setQueryData(keys.tasks.detail(id), replace(detail));
      updateLists<Task, TaskFilters>(qc, keys.tasks.lists, (list) =>
        list.some((t) => t.id === id) ? list.map(replace) : list,
      );
    },
  },
  session_discovered: {
    keys: ({ session }) => [keys.sessions.detail(session.id), keys.sessions.lists],
    apply: ({ session }, qc) => {
      qc.setQueryData(keys.sessions.detail(session.id), session);
      updateLists<Session, SessionFilters>(qc, keys.sessions.lists, (list, filters) =>
        place(list, session, sessionMatches(filters, session)),
      );
    },
  },
};

export interface PatchResult {
  /** Keys a patch wrote under. */
  touched: QueryKey[];
  /** Keys of patches that threw; the caller refetches them. */
  failed: QueryKey[];
}

export function applyPatches(queryClient: QueryClient, events: readonly { body: EventBody }[]): PatchResult {
  const result: PatchResult = { touched: [], failed: [] };
  for (const { body } of events) {
    if (!Object.hasOwn(patches, body.type)) continue;
    const patch = patches[body.type] as Patch<EventType> | undefined;
    if (patch === undefined) continue;
    const data = body.data as DataOf<EventType>;
    const touched = patch.keys(data);
    try {
      patch.apply(data, queryClient);
      result.touched.push(...touched);
    } catch (error) {
      console.warn(`pitcrew: could not apply ${body.type}; refetching instead`, error);
      result.failed.push(...touched);
    }
  }
  return result;
}
