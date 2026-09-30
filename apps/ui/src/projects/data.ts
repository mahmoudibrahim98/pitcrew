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
  type Api,
  type Ask,
  type AskFilters,
  type BriefTarget,
  type Dispatch,
  type Event,
  type MachineId,
  type Member,
  type MemberId,
  type Priority,
  type ProjectId,
  type Receipt,
  type SessionId,
  type Subtask,
  type Task,
  type TaskFilters,
  type TaskId,
  type TaskStatus,
  type TimestampMs,
  type WorkstreamId,
  type CalendarDate,
} from '../data/index.ts';
import { plainNames, type Names } from './format.ts';

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

// ─── Wire types the data layer does not have yet (see "Contract notes" in README.md) ────────────

/** `Brief` in `crates/protocol`: "Where it stands", as in force. */
export interface Brief {
  target: BriefTarget;
  text: string;
  next?: string;
  pinned: boolean;
  source: 'person' | 'back_office';
  updated: TimestampMs;
  receipts: Receipt[];
}

/** `GET /v1/events`: oldest first; `from_rev`/`to_rev` are 0 for an empty page. */
export interface ActivityPage {
  events: Event[];
  from_rev: number;
  to_rev: number;
  at_start: boolean;
}

export interface EventFilters {
  project?: ProjectId;
  workstream?: WorkstreamId;
  task?: TaskId;
  session?: SessionId;
}

export interface NewTask {
  project: ProjectId;
  workstream?: WorkstreamId;
  title: string;
  description?: string;
  status?: TaskStatus;
  priority?: Priority;
  assignee?: MemberId;
  labels?: string[];
  due?: CalendarDate;
}

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

/** The newest `limit` matching events, oldest first, in as few requests as the 500 cap allows. */
export async function fetchActivity(
  api: Api,
  filters: EventFilters,
  limit: number,
  signal?: AbortSignal,
): Promise<ActivityPage> {
  const get = (before: number | undefined, n: number) =>
    api.request<ActivityPage>('GET', '/v1/events', {
      query: eventQuery(filters, before, Math.min(n, EVENTS_MAX_LIMIT)),
      signal,
    });
  const newest = await get(undefined, limit);
  let { events, from_rev, at_start } = newest;
  while (events.length < limit && !at_start && from_rev > 0) {
    const older = await get(from_rev, limit - events.length);
    if (older.events.length === 0) break;
    events = [...older.events, ...events];
    from_rev = older.from_rev;
    at_start = older.at_start;
  }
  return { events, from_rev, to_rev: newest.to_rev, at_start };
}

/**
 * Activity (`GET /v1/events`), newest `pages × 50` events. Keyed under `['events']`, which every
 * stream event invalidates, so the feed stays live; "load older" asks for one more page, and the
 * whole window refetches in one request, so pages never drift apart as new events arrive.
 */
export function useActivity(filters: EventFilters = {}) {
  const api = useApi();
  const [pages, setPages] = useState(1);
  const limit = pages * ACTIVITY_PAGE;
  const filterKey = JSON.stringify(filters);
  const query = useLiveQuery({
    queryKey: [...keys.events, 'list', filters, { limit }],
    queryFn: ({ signal }) => fetchActivity(api, filters, limit, signal),
    // While a bigger window loads, keep showing the smaller one (same filters only).
    placeholderData: (previous, previousQuery) =>
      previousQuery !== undefined && JSON.stringify(previousQuery.queryKey[2]) === filterKey ? previous : undefined,
  });
  return { ...query, loadOlder: () => setPages((n) => n + 1) };
}

/**
 * The revision of each event in an **unfiltered** page (revisions are contiguous there; with
 * filters they are not).
 */
export function withRevisions(page: ActivityPage): { event: Event; rev: number }[] {
  const first = page.to_rev - page.events.length + 1;
  return page.events.map((event, i) => ({ event, rev: first + i }));
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
  });
}

export function useCreateTask() {
  const api = useApi();
  return useMutation({
    mutationFn: (task: NewTask) => api.request<Task>('POST', '/v1/tasks', { body: task }),
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

/** A person's edit of "Where it stands"; pinning is an edit with `pinned` set. */
export function useSaveBrief() {
  const api = useApi();
  return useMutation({
    mutationFn: ({ target, text, next, pinned }: { target: BriefTarget; text: string; next?: string; pinned: boolean }) =>
      api.request<Brief>('PUT', `/v1/briefs/${target.kind}/${id(target.id)}`, { body: { text, next, pinned } }),
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
