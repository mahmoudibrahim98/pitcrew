// Recaps: activity blocks with their one-line summaries, and day paragraphs (API v1, "Recaps").
// Derived from the event log, never stored; paged backwards and kept live like everything else in
// this layer, but with rules of their own (see the contract's "Recaps", "Live updates").
//
// - `clauses()` turns a `Summary` into the text and spans a feature can render, converting the
//   contract's UTF-8 byte ranges to JavaScript string slices.
// - `useRecapBlocks()` and `useRecapDays()` page backwards with `useLiveInfiniteQuery`, by the
//   last block's id and the last day's date.
// - `recapScopeForEvents()` and the cache-free `recapScopeMap` compute what a batch of events
//   touches; `live.ts` turns that into the query keys to invalidate (`recapCacheLookup()`,
//   `recapKeysForScope()`), since that step needs the query cache to know which filtered recap
//   queries are even mounted.

import { keys } from './keys.ts';
import { useApi, useLiveInfiniteQuery } from './provider.tsx';
import type {
  AskId,
  BlocksPage,
  DaysPage,
  DispatchId,
  Event,
  EventType,
  ProjectId,
  Receipt,
  RecapBlockFilters,
  RecapDayScope,
  SessionId,
  Summary,
  TaskId,
  WorkstreamId,
} from './types.ts';

// ─── Clauses ────────────────────────────────────────────────────────────────────────────────────

/** One segment of a summary's text: a clause with its receipts, or the plain text joining two. */
export interface Clause {
  text: string;
  /** Empty for the joining text between clauses (only punctuation and spaces, per the contract). */
  receipts: Receipt[];
}

/**
 * `summary.text` split into its clauses and the text joining them, in order, so a feature can
 * render the whole paragraph with the clauses marked. `span.range` is a **UTF-8 byte range**, on
 * character boundaries (API v1, "Spans are UTF-8 byte ranges"): slicing the JavaScript string
 * (UTF-16) with it directly is wrong as soon as the text holds a character outside ASCII, so this
 * converts through `TextEncoder`/`TextDecoder` instead.
 */
export function clauses(summary: Summary): Clause[] {
  const bytes = new TextEncoder().encode(summary.text);
  const decoder = new TextDecoder();
  const slice = (start: number, end: number): string => decoder.decode(bytes.subarray(start, end));
  const out: Clause[] = [];
  let pos = 0;
  for (const { range, receipts } of summary.spans) {
    if (range.start > pos) out.push({ text: slice(pos, range.start), receipts: [] });
    out.push({ text: slice(range.start, range.end), receipts });
    pos = range.end;
  }
  if (pos < bytes.length) out.push({ text: slice(pos, bytes.length), receipts: [] });
  return out;
}

// ─── Scope: what a batch of events touches, for live invalidation ─────────────────────────────────
//
// API v1, "Recaps", "Live updates": an event's scope is the session, task, workstream and project
// it names, plus their parents from the cache. `live.ts` turns this into the keys to invalidate: the
// unfiltered blocks key always, plus every recap key whose own filter value is in scope.

/** The ids a batch of events touches, grouped by kind (a `session_linked` or a `task_updated` that
 * moves a task between workstreams can put two workstreams, and two projects, in scope at once). */
export interface RecapScope {
  session: readonly SessionId[];
  task: readonly TaskId[];
  workstream: readonly WorkstreamId[];
  project: readonly ProjectId[];
}

const NONE_SCOPE: RecapScope = { session: [], task: [], workstream: [], project: [] };

/**
 * What the query cache knows about an entity's parents, for recap scope resolution. `undefined`
 * means the entity itself is not cached (its parents are then unknown, not merely absent): the
 * caller must fall back to invalidating every recap key.
 */
export interface RecapCacheLookup {
  session(id: SessionId): { task?: TaskId | undefined; workstream?: WorkstreamId | undefined } | undefined;
  task(id: TaskId): { workstream?: WorkstreamId | undefined; project: ProjectId } | undefined;
  workstream(id: WorkstreamId): { project: ProjectId } | undefined;
  dispatch(id: DispatchId): { task: TaskId; session?: SessionId | undefined } | undefined;
  ask(id: AskId): { task?: TaskId | undefined; session?: SessionId | undefined } | undefined;
}

interface Direct {
  session?: SessionId | undefined;
  task?: TaskId | undefined;
  workstream?: WorkstreamId | undefined;
  project?: ProjectId | undefined;
}

function scope(direct: Direct): RecapScope {
  return {
    session: direct.session === undefined ? [] : [direct.session],
    task: direct.task === undefined ? [] : [direct.task],
    workstream: direct.workstream === undefined ? [] : [direct.workstream],
    project: direct.project === undefined ? [] : [direct.project],
  };
}

function dedupe<T>(values: readonly T[]): T[] {
  return [...new Set(values)];
}

function mergeScope(a: RecapScope, b: RecapScope): RecapScope {
  return {
    session: dedupe([...a.session, ...b.session]),
    task: dedupe([...a.task, ...b.task]),
    workstream: dedupe([...a.workstream, ...b.workstream]),
    project: dedupe([...a.project, ...b.project]),
  };
}

/** Unions scopes, short-circuiting on 'everything' (any lookup here could not resolve a link). */
function union(parts: readonly (RecapScope | 'everything')[]): RecapScope | 'everything' {
  let acc = NONE_SCOPE;
  for (const part of parts) {
    if (part === 'everything') return 'everything';
    acc = mergeScope(acc, part);
  }
  return acc;
}

/** A task, plus its workstream and project from the cache. */
function scopeOfTask(id: TaskId, cache: RecapCacheLookup): RecapScope | 'everything' {
  const found = cache.task(id);
  return found === undefined ? 'everything' : scope({ task: id, workstream: found.workstream, project: found.project });
}

/** A workstream, plus its project from the cache. */
function scopeOfWorkstream(id: WorkstreamId, cache: RecapCacheLookup): RecapScope | 'everything' {
  const found = cache.workstream(id);
  return found === undefined ? 'everything' : scope({ workstream: id, project: found.project });
}

/** A session, plus its task and workstream (and their own parents) from the cache. */
function scopeOfSession(id: SessionId, cache: RecapCacheLookup): RecapScope | 'everything' {
  const found = cache.session(id);
  if (found === undefined) return 'everything';
  const parts: (RecapScope | 'everything')[] = [scope({ session: id })];
  if (found.task !== undefined) parts.push(scopeOfTask(found.task, cache));
  if (found.workstream !== undefined) parts.push(scopeOfWorkstream(found.workstream, cache));
  return union(parts);
}

type DataOf<T extends EventType> = Extract<Event['body'], { type: T }>['data'];

/**
 * One entry per event type: 'excluded' for the six kinds the contract says are not activity,
 * 'everything' for `member_added` (it may rename someone a line names), and otherwise this
 * event's scope (empty when it names nothing recaps track, e.g. a `decision_recorded` without a
 * workstream). The mapped type makes a missing entry a compile error.
 */
type RecapScopeMap = {
  [T in EventType]: (data: DataOf<T>, cache: RecapCacheLookup) => 'excluded' | 'everything' | RecapScope;
};

export const recapScopeMap: RecapScopeMap = {
  // Not activity: they change no recap (API v1, "Recaps", "Live updates").
  machine_added: () => 'excluded',
  cursor_moved: () => 'excluded',
  machine_liveness: () => 'excluded',
  persona_saved: () => 'excluded',
  team_saved: () => 'excluded',
  project_created: () => 'excluded',
  brief_proposed: () => 'excluded',
  // May rename someone a line names.
  member_added: () => 'everything',

  session_discovered: (d, cache) => {
    const { session } = d;
    const parts: (RecapScope | 'everything')[] = [
      scope({ session: session.id, task: session.task, workstream: session.workstream }),
    ];
    if (session.task !== undefined) parts.push(scopeOfTask(session.task, cache));
    if (session.workstream !== undefined) parts.push(scopeOfWorkstream(session.workstream, cache));
    return union(parts);
  },
  session_state_changed: (d, cache) => scopeOfSession(d.session, cache),
  turn_ended: (d, cache) => scopeOfSession(d.session, cache),
  tool_ran: (d, cache) => scopeOfSession(d.session, cache),
  file_edited: (d, cache) => scopeOfSession(d.session, cache),
  session_updated: (d, cache) => scopeOfSession(d.session, cache),
  // Both the old link (from the cache, still current when this is resolved) and the new one.
  session_linked: (d, cache) => {
    const old = scopeOfSession(d.session, cache);
    const parts: (RecapScope | 'everything')[] = [old, scope({ session: d.session, task: d.task, workstream: d.workstream })];
    if (d.workstream !== undefined) parts.push(scopeOfWorkstream(d.workstream, cache));
    if (d.task !== undefined) parts.push(scopeOfTask(d.task, cache));
    return union(parts);
  },
  session_ended: (d, cache) => scopeOfSession(d.session, cache),

  // The full object is in the event: no cache lookup needed (and a brand-new workstream would
  // not be cached yet).
  workstream_created: (d) => scope({ workstream: d.workstream.id, project: d.workstream.project }),
  workstream_changed: (d, cache) => scopeOfWorkstream(d.workstream, cache),
  workstream_linked: (d, cache) => scopeOfWorkstream(d.workstream, cache),

  task_created: (d) => scope({ task: d.task.id, workstream: d.task.workstream, project: d.task.project }),
  task_moved: (d, cache) => scopeOfTask(d.task, cache),
  task_assigned: (d, cache) => scopeOfTask(d.task, cache),
  // The old workstream (from the cache) always; the new one too when the patch sets one.
  task_updated: (d, cache) => {
    const base = scopeOfTask(d.task, cache);
    return typeof d.patch.workstream === 'string' ? union([base, scopeOfWorkstream(d.patch.workstream, cache)]) : base;
  },
  subtasks_replaced: (d, cache) => scopeOfTask(d.task, cache),

  dispatch_started: (d, cache) => {
    const parts: (RecapScope | 'everything')[] = [scopeOfTask(d.dispatch.task, cache)];
    if (d.dispatch.session !== undefined) parts.push(scope({ session: d.dispatch.session }));
    return union(parts);
  },
  dispatch_finished: (d, cache) => {
    const info = cache.dispatch(d.dispatch);
    if (info === undefined) return 'everything';
    const parts: (RecapScope | 'everything')[] = [scopeOfTask(info.task, cache)];
    if (info.session !== undefined) parts.push(scope({ session: info.session }));
    return union(parts);
  },

  ask_raised: (d, cache) => {
    const { ask } = d;
    const parts: (RecapScope | 'everything')[] = [];
    if (ask.task !== undefined) parts.push(scopeOfTask(ask.task, cache));
    if (ask.session !== undefined) parts.push(scope({ session: ask.session }));
    return union(parts);
  },
  ask_answered: (d, cache) => {
    const info = cache.ask(d.ask);
    if (info === undefined) return 'everything';
    const parts: (RecapScope | 'everything')[] = [];
    if (info.task !== undefined) parts.push(scopeOfTask(info.task, cache));
    if (info.session !== undefined) parts.push(scope({ session: info.session }));
    return union(parts);
  },

  comment_posted: (d, cache) => {
    const parts: (RecapScope | 'everything')[] = [];
    if (d.task !== undefined) parts.push(scopeOfTask(d.task, cache));
    if (d.workstream !== undefined) parts.push(scopeOfWorkstream(d.workstream, cache));
    return union(parts);
  },
  safety_changed: () => 'excluded',
  brief_accepted: (d, cache) => (d.target.kind === 'project' ? scope({ project: d.target.id }) : scopeOfWorkstream(d.target.id, cache)),
  decision_recorded: (d, cache) => (d.workstream === undefined ? NONE_SCOPE : scopeOfWorkstream(d.workstream, cache)),
  // Outward writes are about their task.
  write_proposed: (d, cache) => (d.write.task === undefined ? NONE_SCOPE : scopeOfTask(d.write.task, cache)),
  write_started: (d, cache) => (d.task === undefined ? NONE_SCOPE : scopeOfTask(d.task, cache)),
  write_finished: (d, cache) => (d.task === undefined ? NONE_SCOPE : scopeOfTask(d.task, cache)),
};

/**
 * The scope of a batch of events: `undefined` when none of them are activity (nothing to
 * invalidate), `'everything'` when any of them cannot be resolved (an unknown event type, data
 * that does not match its type, or `member_added`), otherwise their union. Never throws: a
 * malformed event is handled the same way as an unresolved cache link — always correct, only
 * slower — rather than escaping to the stream's own malformed-event handling (which already
 * refetches everything and logs once).
 */
export function recapScopeForEvents(events: readonly Event[], cache: RecapCacheLookup): RecapScope | 'everything' | undefined {
  let acc: RecapScope | undefined;
  for (const event of events) {
    const entry = Object.hasOwn(recapScopeMap, event.body.type)
      ? (recapScopeMap as Record<string, (data: unknown, cache: RecapCacheLookup) => 'excluded' | 'everything' | RecapScope>)[
          event.body.type
        ]
      : undefined;
    let result: 'excluded' | 'everything' | RecapScope;
    try {
      result = entry === undefined ? 'everything' : entry(event.body.data, cache);
    } catch {
      result = 'everything';
    }
    if (result === 'excluded') continue;
    if (result === 'everything') return 'everything';
    acc = acc === undefined ? result : mergeScope(acc, result);
  }
  return acc;
}

// ─── Hooks ──────────────────────────────────────────────────────────────────────────────────────

export interface RecapBlocksOptions {
  /** Blocks per page; the API's default (50) when absent. Mainly for tests. */
  limit?: number;
}

function blocksKey(filters: RecapBlockFilters, limit: number | undefined) {
  const key = keys.recaps.blocks(filters);
  return limit === undefined ? key : [...key, { limit }];
}

/**
 * Activity blocks matching `filters`, newest first, paged backwards by the last block's id
 * (`BlocksPage.at_start` ends paging). `blocks`/`atStart` flatten the loaded pages; `loadMore()`
 * is a no-op while a page is already loading or none is left.
 */
export function useRecapBlocks(filters: RecapBlockFilters = {}, options: RecapBlocksOptions = {}) {
  const api = useApi();
  const { limit } = options;
  const query = useLiveInfiniteQuery({
    queryKey: blocksKey(filters, limit),
    queryFn: ({ pageParam, signal }) => api.recapBlocks(filters, pageParam, limit, signal),
    initialPageParam: undefined as string | undefined,
    getNextPageParam: (lastPage: BlocksPage) => (lastPage.at_start ? undefined : lastPage.blocks.at(-1)?.block.id),
  });
  return {
    ...query,
    blocks: query.data?.pages.flatMap((page) => page.blocks) ?? [],
    atStart: query.data?.pages.at(-1)?.at_start ?? false,
    loadMore(): void {
      if (query.hasNextPage && !query.isFetchingNextPage) void query.fetchNextPage();
    },
  };
}

export interface RecapDaysOptions {
  /**
   * Minutes east of UTC, overriding the viewer's own offset (`-new Date().getTimezoneOffset()`).
   * The mock hub serves `tz=0` only, so e2e suites (and anything run against it) must pass `0`.
   */
  tz?: number;
  /** Dates per page; the API's default (7) when absent. Mainly for tests. */
  limit?: number;
}

function daysKey(scope: RecapDayScope, tz: number, limit: number | undefined) {
  const key = keys.recaps.days(scope, tz);
  return limit === undefined ? key : [...key, { limit }];
}

/**
 * Day paragraphs for `scope` (a workstream or a project), newest date first, paged backwards by
 * the last entry's date (`DaysPage.at_start` ends paging). `days`/`atStart` flatten the loaded
 * pages; `loadMore()` is a no-op while a page is already loading or none is left.
 */
export function useRecapDays(scope: RecapDayScope, options: RecapDaysOptions = {}) {
  const api = useApi();
  const { limit } = options;
  const tz = options.tz ?? -new Date().getTimezoneOffset();
  const query = useLiveInfiniteQuery({
    queryKey: daysKey(scope, tz, limit),
    queryFn: ({ pageParam, signal }) => api.recapDays(scope, tz, pageParam, limit, signal),
    initialPageParam: undefined as string | undefined,
    getNextPageParam: (lastPage: DaysPage) => (lastPage.at_start ? undefined : lastPage.days.at(-1)?.date),
  });
  return {
    ...query,
    days: query.data?.pages.flatMap((page) => page.days) ?? [],
    atStart: query.data?.pages.at(-1)?.at_start ?? false,
    loadMore(): void {
      if (query.hasNextPage && !query.isFetchingNextPage) void query.fetchNextPage();
    },
  };
}
