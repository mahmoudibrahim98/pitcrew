// Data hooks for the projects layout, on the data layer's `useLiveQuery` (see src/data/README.md).
// Reads wait for the stream; writes never touch the cache, the events they cause do.

import { useMutation } from '@tanstack/react-query';
import { useState } from 'react';
import {
  keys,
  useApi,
  useLiveQuery,
  useMe,
  useMembers,
  useProjects,
  useSessions,
  useTasks,
  useWorkstreams,
  type ActivityPage,
  type Api,
  type Ask,
  type AskFilters,
  type Brief,
  type BriefTarget,
  type Dispatch,
  type Event,
  type EventFilters,
  type MachineId,
  type Member,
  type MemberId,
  type NewTask,
  type Subtask,
  type Task,
  type TaskFilters,
  type TaskId,
  type WorkstreamId,
} from '../data/index.ts';
import { plainNames, type Names } from './format.ts';
import { toast } from '../design/toast.tsx';
import { taskLink, useProjectsNav } from './nav.tsx';
import type { TaskPatch } from '../data/index.ts';

export {
  useMe,
  useMembers,
  useMoveTask,
  useProject,
  useProjects,
  useSessions,
  useTask,
  useTasks,
  useWorkstream,
  useWorkstreams,
} from '../data/index.ts';

// The wire types now live in the data layer; these names are what the projects code uses.
export type { ActivityPage, Brief, EventFilters, NewTask } from '../data/index.ts';

const id = encodeURIComponent;

// ─── Reads ──────────────────────────────────────────────────────────────────────────────────────

/** A workstream by id, or nothing (and no request) without one. */
export function useOptionalWorkstream(workstream: WorkstreamId | undefined) {
  const api = useApi();
  return useLiveQuery({
    queryKey: keys.workstreams.detail(workstream ?? ''),
    queryFn: ({ signal }) => api.workstream(workstream ?? '', signal),
    enabled: workstream !== undefined,
  });
}

export function useMachines() {
  const api = useApi();
  return useLiveQuery({ queryKey: keys.machines, queryFn: ({ signal }) => api.machines(signal) });
}

/** Open asks addressed to me: the Inbox, and the source of every "Needs you" badge. */
export function useInbox() {
  const api = useApi();
  const me = useMe();
  const to = me.data?.id;
  const filters: AskFilters = to === undefined ? { state: 'open' } : { to, state: 'open' };
  return useLiveQuery({
    queryKey: keys.asks.list(filters),
    queryFn: ({ signal }) => api.asks(filters, signal),
    enabled: to !== undefined,
  });
}

/** Every ask, whatever its state, for naming asks in activity. */
export function useAllAsks() {
  const api = useApi();
  return useLiveQuery({ queryKey: keys.asks.list({}), queryFn: ({ signal }) => api.asks({}, signal) });
}

export function useBriefs() {
  const api = useApi();
  return useLiveQuery({
    queryKey: keys.briefs,
    queryFn: ({ signal }) => api.request<Brief[]>('GET', '/v1/briefs', { signal }),
  });
}

export function sameTarget(a: BriefTarget, b: BriefTarget): boolean {
  return a.kind === b.kind && a.id === b.id;
}

/** The brief in force for a project or workstream, if it has one. */
export function useBrief(target: BriefTarget) {
  const briefs = useBriefs();
  return { ...briefs, brief: briefs.data?.find((b) => sameTarget(b.target, target)) };
}

/** Events per "page" of activity. */
export const ACTIVITY_PAGE = 50;
const EVENTS_MAX_LIMIT = 500;

function eventQuery(filters: EventFilters, before: number | undefined, limit: number) {
  return {
    project: filters.project,
    workstream: filters.workstream,
    task: filters.task,
    session: filters.session,
    before: before === undefined ? undefined : String(before),
    limit: String(limit),
  };
}

/**
 * Requests one call may spend looking past what is already shown. With filters the hub scans a
 * bounded window per request, so a sparse feed can answer with empty pages for a while.
 */
export const ACTIVITY_BUDGET = 8;
/** A safety cap on the requests that re-cover what is already shown. */
const COVER_CAP = 40;

/**
 * A window of activity, newest first in the feed and oldest first in `events`: every matching
 * event from the newest down to `reach` (a `from_rev` an earlier call returned), then up to one
 * page more, within `ACTIVITY_BUDGET` requests.
 *
 * Only `at_start` ends the feed. An empty page that is not at the start says where its scan
 * stopped (`from_rev`), and the next request carries on from there. The returned `from_rev` is
 * where to resume: pass it as `reach` to get the next page too.
 */
export async function fetchActivity(
  api: Api,
  filters: EventFilters,
  reach: number | undefined,
  signal?: AbortSignal,
): Promise<ActivityPage> {
  const get = (before: number | undefined, n: number) =>
    api.request<ActivityPage>('GET', '/v1/events', {
      query: eventQuery(filters, before, Math.min(n, EVENTS_MAX_LIMIT)),
      signal,
    });
  let events: Event[] = [];
  let revisions: number[] = [];
  let before: number | undefined;
  let toRev = 0;
  let atStart = false;
  let covering = 0;
  let spent = 0;
  let found = 0;
  for (;;) {
    const past = reach === undefined || (before !== undefined && before <= reach);
    const done = past ? found >= ACTIVITY_PAGE || spent >= ACTIVITY_BUDGET : covering >= COVER_CAP;
    if (atStart || done) break;
    const page = await get(before, past ? ACTIVITY_PAGE - found : EVENTS_MAX_LIMIT);
    if (past) {
      spent += 1;
      found += page.events.filter((event) => event.body.type !== 'cursor_moved').length;
    } else {
      covering += 1;
    }
    const real = withRevisions(page).filter(({ event }) => event.body.type !== 'cursor_moved');
    events = [...real.map(({ event }) => event), ...events];
    revisions = [...real.map(({ rev }) => rev), ...revisions];
    if (toRev === 0 && real.length > 0) toRev = real.at(-1)?.rev ?? 0;
    atStart = page.at_start;
    // A page that is not at the start but gives no older position cannot be continued: stop
    // there rather than ask for the same thing again.
    if (!atStart && (page.from_rev <= 0 || (before !== undefined && page.from_rev >= before))) {
      atStart = true;
    }
    before = page.from_rev;
  }
  return { events, revisions, from_rev: before ?? 0, to_rev: toRev, at_start: atStart };
}

/**
 * Activity (`GET /v1/events`) as one live window, keyed under `['events']` (every stream event
 * invalidates it, so the feed stays current). "Load older" moves the window's `reach` down to
 * where the last call stopped; the whole window then refetches from the newest event, so older
 * and newer parts never drift apart as events arrive.
 */
export function useActivity(filters: EventFilters = {}) {
  const api = useApi();
  const filterKey = JSON.stringify(filters);
  const [older, setOlder] = useState<{ filters: string; reach: number } | null>(null);
  const reach = older !== null && older.filters === filterKey ? older.reach : undefined;
  const query = useLiveQuery({
    queryKey: [...keys.events, 'list', filters, { reach: reach ?? null }],
    queryFn: ({ signal }) => fetchActivity(api, filters, reach, signal),
    // While a longer window loads, keep showing the shorter one (same filters only).
    placeholderData: (previous, previousQuery) =>
      previousQuery !== undefined && JSON.stringify(previousQuery.queryKey[2]) === filterKey ? previous : undefined,
  });
  const page = query.data;
  const loadOlder = () => {
    if (page !== undefined && !page.at_start && !query.isPlaceholderData) {
      setOlder({ filters: filterKey, reach: page.from_rev });
    }
  };
  return { ...query, loadOlder };
}

/**
 * The actual revision of each event; older hubs omit `revisions`.
 */
export function withRevisions(page: ActivityPage): { event: Event; rev: number }[] {
  const first = page.to_rev - page.events.length + 1;
  return page.events.map((event, i) => ({ event, rev: page.revisions?.[i] ?? first + i }));
}

/** Names for members, tasks, workstreams, projects, sessions and machines, from the cache. */
export function useNames(): Names {
  const members = useMembers();
  const tasks = useTasks();
  const workstreams = useWorkstreams();
  const projects = useProjects();
  const sessions = useSessions();
  const machines = useMachines();
  const asks = useAllAsks();
  const byId = <T extends { id: string }>(list: T[] | undefined) => new Map((list ?? []).map((x) => [x.id, x]));
  const member = byId(members.data);
  const task = byId(tasks.data);
  const workstream = byId(workstreams.data);
  const project = byId(projects.data);
  const session = byId(sessions.data);
  const machine = byId(machines.data);
  const ask = byId(asks.data);
  return {
    member: (x) => member.get(x)?.handle ?? plainNames.member(x),
    task: (x) => task.get(x)?.key ?? plainNames.task(x),
    workstream: (x) => workstream.get(x)?.name ?? plainNames.workstream(x),
    project: (x) => project.get(x)?.name ?? plainNames.project(x),
    session: (x) => session.get(x)?.title ?? plainNames.session(x),
    machine: (x) => machine.get(x)?.name ?? plainNames.machine(x),
    ask: (x) => ask.get(x)?.title,
  };
}

/** Members by id. */
export function useMemberMap(): Map<MemberId, Member> {
  const members = useMembers();
  return new Map((members.data ?? []).map((m) => [m.id, m]));
}

/** The tasks list keyed by id, for dependencies. */
export function useTaskMap(filters: TaskFilters = {}): Map<TaskId, Task> {
  const tasks = useTasks(filters);
  return new Map((tasks.data ?? []).map((t) => [t.id, t]));
}

// ─── Writes ─────────────────────────────────────────────────────────────────────────────────────

export function useAssignTask() {
  const api = useApi();
  return useMutation({
    mutationFn: ({ task, assignee }: { task: TaskId; assignee: MemberId | null }) =>
      api.request<Task>('POST', `/v1/tasks/${id(task)}/assign`, { body: { assignee } }),
    onSuccess: (task) => toast(`${task.key} assigned`),
    onError: taskError,
  });
}

export function useCreateTask() {
  const api = useApi();
  const nav = useProjectsNav();
  return useMutation({
    mutationFn: (task: NewTask) => api.request<Task>('POST', '/v1/tasks', { body: task }),
    onSuccess: (task) => toast(`${task.key} created`, { action: { label: 'Open', run: () => {
      if (nav.openTask !== undefined) nav.openTask(task.id);
      else window.location.assign(taskLink(task.id));
    } } }),
    onError: taskError,
  });
}

export function taskError(error: Error) {
  toast(`${error instanceof Error ? error.message : String(error)} Check the task and try again.`, { error: true });
}

export function usePatchTask() {
  const api = useApi();
  return useMutation({
    mutationFn: ({ task, patch }: { task: TaskId; patch: TaskPatch }) => api.patchTask(task, patch),
    onSuccess: (task, { patch }) => toast(`${task.key} ${patch.archived === true ? 'archived' : patch.archived === false ? 'restored' : 'updated'}`,
      patch.archived === true ? { action: { label: 'Undo', run: async () => {
        await api.patchTask(task.id, { archived: false });
        toast(`${task.key} restored`);
      } } } : {}),
    onError: taskError,
  });
}

export function useDispatches(task: TaskId) {
  const api = useApi();
  return useLiveQuery({
    queryKey: [...keys.dispatches, { task }],
    queryFn: ({ signal }) => api.request<Dispatch[]>('GET', `/v1/tasks/${id(task)}/dispatches`, { signal }),
  });
}

export function useReplaceSubtasks() {
  const api = useApi();
  return useMutation({
    mutationFn: ({ task, subtasks }: { task: TaskId; subtasks: Subtask[] }) =>
      api.request<Task>('PUT', `/v1/tasks/${id(task)}/subtasks`, { body: subtasks }),
  });
}

export function usePostComment() {
  const api = useApi();
  return useMutation({
    mutationFn: ({ task, text, mentions }: { task: TaskId; text: string; mentions: MemberId[] }) =>
      api.request<Event>('POST', `/v1/tasks/${id(task)}/comments`, { body: { text, mentions } }),
  });
}

export function useDispatchTask() {
  const api = useApi();
  return useMutation({
    mutationFn: ({
      task,
      agent,
      brief,
      machine,
    }: {
      task: TaskId;
      agent: MemberId;
      brief?: string;
      machine?: MachineId;
    }) => api.request<Dispatch>('POST', `/v1/tasks/${id(task)}/dispatch`, { body: { agent, brief, machine } }),
  });
}

export function useAnswerAsk() {
  const api = useApi();
  return useMutation({
    mutationFn: ({ ask, option, text }: { ask: Ask['id']; option?: number; text?: string }) =>
      api.request<Ask>('POST', `/v1/asks/${id(ask)}/answer`, { body: { option, text } }),
  });
}

/**
 * A person's edit of "Where it stands"; pinning, and "Keep current" (which clears any pending
 * proposal), are an edit with the same text.
 */
export function useSaveBrief() {
  const api = useApi();
  return useMutation({
    mutationFn: ({ target, text, next, pinned }: { target: BriefTarget; text: string; next?: string; pinned: boolean }) =>
      api.editBrief(target, { text, pinned, ...(next === undefined ? {} : { next }) }),
  });
}

/** Accepts the back office's pending proposal: its text and next step become the brief in force. */
export function useAcceptBrief() {
  const api = useApi();
  return useMutation({
    mutationFn: ({
      target,
      proposal,
      pinned,
    }: {
      target: BriefTarget;
      proposal: { text: string; next?: string };
      pinned: boolean;
    }) => api.acceptBrief(target, proposal, pinned),
  });
}

// ─── Helpers ────────────────────────────────────────────────────────────────────────────────────

/** Members whose handle appears as an `@mention` in the text. */
export function mentionsIn(text: string, members: readonly Member[]): MemberId[] {
  const handles = new Set(Array.from(text.matchAll(/(?:^|[^\w@])(@[\w.-]*\w)/g), (m) => (m[1] ?? '').toLowerCase()));
  return members.filter((m) => handles.has(m.handle.toLowerCase())).map((m) => m.id);
}

const CROCKFORD = '0123456789ABCDEFGHJKMNPQRSTVWXYZ';

/** A new ULID for ids the client supplies (subtasks). */
export function newUlid(now: number = Date.now()): string {
  let time = '';
  let t = now;
  for (let i = 0; i < 10; i++) {
    time = CROCKFORD.charAt(t % 32) + time;
    t = Math.floor(t / 32);
  }
  let random = '';
  for (const byte of crypto.getRandomValues(new Uint8Array(16))) random += CROCKFORD.charAt(byte % 32);
  return time + random;
}
