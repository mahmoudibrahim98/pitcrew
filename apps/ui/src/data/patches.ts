// Events that carry the whole object are written straight into the cache instead of refetching.
//
// List data under `['tasks', 'list', filters]` and `['sessions', 'list', filters]` must be plain
// arrays of the wire objects; the filters are the key's third element.
//
// A write marks the query fresh, so queries already invalidated are left alone: they refetch
// anyway, and a write would cancel that refetch. Lists the event does not change are not written.

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

/** Whether a write under `key` would hide a pending refetch. */
function invalidated(queryClient: QueryClient, key: QueryKey): boolean {
  return queryClient.getQueryState(key)?.isInvalidated === true;
}

/** Writes a detail unless it is invalidated; `update` gets the cached value, if any. */
function updateDetail<T>(queryClient: QueryClient, key: QueryKey, update: (old: T | undefined) => T | undefined): void {
  if (invalidated(queryClient, key)) return;
  const next = update(queryClient.getQueryData<T>(key));
  if (next !== undefined) queryClient.setQueryData(key, next);
}

/**
 * Updates every cached list under `prefix`, skipping invalidated lists and lists `update` returns
 * unchanged. Throws on data that is not an array.
 */
function updateLists<T, F>(
  queryClient: QueryClient,
  prefix: QueryKey,
  update: (list: T[], filters: F) => T[],
): void {
  for (const query of queryClient.getQueryCache().findAll({ queryKey: prefix })) {
    const { queryKey } = query;
    const list: unknown = query.state.data;
    if (list === undefined || query.state.isInvalidated) continue;
    if (!Array.isArray(list)) throw new Error(`${JSON.stringify(queryKey)} does not hold a list`);
    const next = update(list as T[], (queryKey[2] ?? {}) as F);
    if (next !== list) queryClient.setQueryData(queryKey, next);
  }
}

function rediscovered(old: Session | undefined, incoming: Session): Session {
  if (old === undefined) return incoming;
  const firm = (basis: Session['link_basis']) =>
    basis !== undefined && basis !== null && ['dispatch', 'manual', 'claimed', 'imported'].includes(basis);
  const next = { ...incoming };
  if (old.state === 'ended') {
    next.state = 'ended';
    delete next.status_line;
    if (old.status_line !== undefined) next.status_line = old.status_line;
  }
  if (firm(old.link_basis) && !firm(incoming.link_basis)) {
    delete next.workstream;
    delete next.task;
    delete next.link_basis;
    if (old.workstream !== undefined) next.workstream = old.workstream;
    if (old.task !== undefined) next.task = old.task;
    if (old.link_basis !== undefined) next.link_basis = old.link_basis;
  }
  if (next.agent === undefined && old.agent !== undefined) next.agent = old.agent;
  // As the hub does: a model or account once recorded stays until a re-statement names another.
  if (old.recorded !== undefined) next.recorded = { ...old.recorded, ...incoming.recorded };
  return next;
}

function changeSession(qc: QueryClient, id: string, change: (s: Session) => Session) {
  updateDetail<Session>(qc, keys.sessions.detail(id), (old) => old && change(old));
  updateLists<Session, SessionFilters>(qc, keys.sessions.lists, (list, filters) => {
    const old = list.find((s) => s.id === id);
    if (old === undefined) return list;
    const next = change(old);
    return place(list, next, sessionMatches(filters, next));
  });
}

export const patches: PatchMap = {
  task_created: {
    keys: ({ task }) => [keys.tasks.detail(task.id), keys.tasks.lists],
    apply: ({ task }, qc) => {
      updateDetail<Task>(qc, keys.tasks.detail(task.id), () => task);
      updateLists<Task, TaskFilters>(qc, keys.tasks.lists, (list, filters) =>
        place(list, task, taskMatches(filters, task)),
      );
    },
  },
  subtasks_replaced: {
    keys: ({ task }) => [keys.tasks.detail(task), keys.tasks.lists],
    apply: ({ task: id, subtasks }, qc) => {
      const replace = (task: Task): Task => (task.id === id ? { ...task, subtasks } : task);
      updateDetail<Task>(qc, keys.tasks.detail(id), (detail) => detail && replace(detail));
      updateLists<Task, TaskFilters>(qc, keys.tasks.lists, (list) =>
        list.some((t) => t.id === id) ? list.map(replace) : list,
      );
    },
  },
  session_ended: {
    keys: ({ session }) => [keys.sessions.detail(session), keys.sessions.lists],
    apply: ({ session }, qc) => changeSession(qc, session, (s) => ({ ...s, state: 'ended' })),
  },
  session_state_changed: {
    keys: ({ session }) => [keys.sessions.detail(session), keys.sessions.lists],
    apply: ({ session, to, status_line }, qc) => changeSession(qc, session, (s) => {
      const next = { ...s, state: to };
      delete next.status_line;
      if (status_line !== undefined) next.status_line = status_line;
      return next;
    }),
  },
  session_discovered: {
    keys: ({ session }) => [keys.sessions.detail(session.id), keys.sessions.lists],
    apply: ({ session }, qc) => {
      const cached = qc.getQueryData<Session>(keys.sessions.detail(session.id)) ??
        qc.getQueriesData<Session[]>({ queryKey: keys.sessions.lists }).flatMap(([, list]) => list ?? []).find((s) => s.id === session.id);
      updateDetail<Session>(qc, keys.sessions.detail(session.id), (old) => rediscovered(old, session));
      updateLists<Session, SessionFilters>(qc, keys.sessions.lists, (list, filters) => {
        const next = rediscovered(list.find((s) => s.id === session.id) ?? cached, session);
        return place(list, next, sessionMatches(filters, next));
      });
    },
  },
};

export interface PatchResult {
  /** Keys a patch wrote under. */
  touched: QueryKey[];
  /** Keys of patches that threw; the caller refetches them (`[]`, everything, if unknown). */
  failed: QueryKey[];
}

export function applyPatches(queryClient: QueryClient, events: readonly { body: EventBody }[]): PatchResult {
  const result: PatchResult = { touched: [], failed: [] };
  for (const { body } of events) {
    if (!Object.hasOwn(patches, body.type)) continue;
    const patch = patches[body.type] as Patch<EventType> | undefined;
    if (patch === undefined) continue;
    const data = body.data as DataOf<EventType>;
    let touched: QueryKey[] | undefined;
    try {
      touched = patch.keys(data);
      patch.apply(data, queryClient);
      result.touched.push(...touched);
    } catch (error) {
      console.warn(`pitcrew: could not apply ${body.type}; refetching instead`, error);
      // Without the keys (malformed data), refetch everything; the caller rate-limits that.
      result.failed.push(...(touched ?? [[]]));
    }
  }
  return result;
}
