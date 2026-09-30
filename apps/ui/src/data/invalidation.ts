// Event → query keys. On each event from the stream, the UI invalidates exactly the keys the
// event touches (API v1, "Client rule"). Every event also invalidates the activity feed. Events
// that carry the whole object are applied by patches.ts instead, so their entries here are
// what the patch cannot cover.

import { keys } from './keys.ts';
import type { Event, EventBody, EventType, TaskId, WorkstreamId } from './types.ts';

export type QueryKey = readonly unknown[];

/** What the map may look up in the cache, for events that name a task but not its workstream. */
export interface CacheLookup {
  taskWorkstream(task: TaskId): WorkstreamId | undefined;
}

type DataOf<T extends EventType> = Extract<EventBody, { type: T }>['data'];

/**
 * One entry per event type; the mapped type makes a missing entry a compile error. Features do not
 * extend it at run time: they ask stream L for entries (see README.md, "Rules for features").
 */
export type InvalidationMap = {
  [T in EventType]: (data: DataOf<T>, cache: CacheLookup) => QueryKey[];
};

const session = (id: string): QueryKey[] => [keys.sessions.detail(id), keys.sessions.lists];

const task = (id: string): QueryKey[] => [keys.tasks.detail(id), keys.tasks.lists];

const taskAndWorkstream = (id: string, cache: CacheLookup): QueryKey[] => {
  const workstream = cache.taskWorkstream(id);
  return workstream === undefined ? task(id) : [...task(id), keys.workstreams.detail(workstream)];
};

export const invalidationMap: InvalidationMap = {
  machine_added: () => [keys.machines],
  // The member may be the signed-in one, whose details changed.
  member_added: () => [keys.members, keys.me],
  persona_saved: () => [keys.personas],
  team_saved: () => [keys.teams],
  machine_liveness: () => [keys.machines],
  session_discovered: () => [],
  // Going to `waiting` usually means a question just landed in the transcript.
  session_state_changed: (d) => [...session(d.session), keys.sessions.transcript(d.session)],
  turn_ended: (d) => [...session(d.session), keys.sessions.transcript(d.session)],
  tool_ran: (d) => [keys.sessions.detail(d.session), keys.sessions.transcript(d.session)],
  file_edited: (d) => [keys.sessions.detail(d.session), keys.sessions.transcript(d.session)],
  session_updated: (d) => session(d.session),
  session_linked: (d) => session(d.session),
  session_ended: (d) => session(d.session),
  project_created: (d) => [keys.projects.lists, keys.projects.detail(d.project.id)],
  workstream_created: (d) => [keys.workstreams.lists, keys.workstreams.detail(d.workstream.id)],
  workstream_changed: (d) => [keys.workstreams.lists, keys.workstreams.detail(d.workstream)],
  task_created: () => [],
  task_moved: (d, cache) => taskAndWorkstream(d.task, cache),
  task_assigned: (d) => task(d.task),
  subtasks_replaced: () => [],
  dispatch_started: (d) => [keys.dispatches, keys.tasks.detail(d.dispatch.task)],
  dispatch_finished: () => [keys.dispatches],
  ask_raised: () => [keys.asks.lists],
  ask_answered: () => [keys.asks.lists],
  comment_posted: () => [],
  brief_proposed: () => [keys.briefs],
  brief_accepted: () => [keys.briefs],
  decision_recorded: () => [],
};

function keysFor(body: EventBody, cache: CacheLookup): QueryKey[] {
  // The map is typed per event; TypeScript cannot correlate `body.type` with `body.data` here.
  const entry = invalidationMap[body.type] as (data: unknown, cache: CacheLookup) => QueryKey[];
  return entry(body.data, cache);
}

/** The distinct keys a batch of events touches, the activity feed included. */
export function keysToInvalidate(events: readonly Event[], cache: CacheLookup): QueryKey[] {
  const seen = new Map<string, QueryKey>();
  const add = (key: QueryKey) => seen.set(JSON.stringify(key), key);
  if (events.length > 0) add(keys.events);
  for (const event of events) {
    // An event type newer than this build: refetch everything rather than show stale data.
    if (!Object.hasOwn(invalidationMap, event.body.type)) return [[]];
    for (const key of keysFor(event.body, cache)) add(key);
  }
  return [...seen.values()];
}
