// The shell's own queries and counts, on the data layer's live queries.

import {
  keys,
  useApi,
  useLiveQuery,
  useMe,
  type Ask,
  type AskFilters,
  type Session,
  type Task,
  type Workstream,
} from '../data/index.ts';

export function isOpenTask(task: Task): boolean {
  return task.status !== 'done' && task.status !== 'canceled';
}

/** Open asks addressed to the signed-in member: the Inbox. */
export function useMyOpenAsks() {
  const api = useApi();
  const me = useMe().data?.id;
  const filters: AskFilters = me === undefined ? { state: 'open' } : { to: me, state: 'open' };
  return useLiveQuery({
    queryKey: keys.asks.list(filters),
    queryFn: ({ signal }) => api.asks(filters, signal),
    enabled: me !== undefined,
  });
}

/** Where each open ask points: its task's workstream and project, or its session's workstream. */
export function askPlaces(
  asks: readonly Ask[],
  tasks: readonly Task[],
  sessions: readonly Session[],
  workstreams: readonly Workstream[],
): { project?: string; workstream?: string }[] {
  const taskById = new Map(tasks.map((t) => [t.id, t]));
  const sessionById = new Map(sessions.map((s) => [s.id, s]));
  const projectOf = new Map(workstreams.map((w) => [w.id, w.project]));
  return asks.map((ask) => {
    const task = ask.task === undefined ? undefined : taskById.get(ask.task);
    if (task !== undefined) {
      return task.workstream === undefined
        ? { project: task.project }
        : { project: task.project, workstream: task.workstream };
    }
    const workstream = ask.session === undefined ? undefined : sessionById.get(ask.session)?.workstream;
    if (workstream === undefined) return {};
    const project = projectOf.get(workstream);
    return project === undefined ? { workstream } : { project, workstream };
  });
}

export function countBy<T>(items: readonly T[], key: (item: T) => string | undefined): Map<string, number> {
  const counts = new Map<string, number>();
  for (const item of items) {
    const k = key(item);
    if (k !== undefined) counts.set(k, (counts.get(k) ?? 0) + 1);
  }
  return counts;
}
