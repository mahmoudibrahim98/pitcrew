// The console's data hooks, on the data layer's `useLiveQuery` and keys (src/data/README.md).
// Lists and details reuse the shared keys, so the stream's patches and invalidations reach them.
// Mutations never touch the cache: the events they cause do.

import { useMutation, useQueryClient } from '@tanstack/react-query';
import { useEffect, useLayoutEffect, useRef, useState } from 'react';
import { create, useStore } from 'zustand';
import { createStore } from 'zustand/vanilla';
import {
  keys,
  useApi,
  useLiveQuery,
  useProjects,
  useSession,
  useSessions,
  useTasks,
  useWorkstreams,
  type Engine,
  type Project,
  type Session,
  type SessionState,
  type Task,
  type Workstream,
} from '../data/index.ts';
import { consoleApiFor, type ConsoleApi } from './api.ts';
import { deliveredPrompts, EMPTY_VIEW, TranscriptWindow, type PendingPrompt, type WindowView } from './transcript.ts';
import type { EndMode, Key, TranscriptPage } from './types.ts';

export function useConsoleApi(): ConsoleApi {
  return consoleApiFor(useApi());
}

// ─── Reads ──────────────────────────────────────────────────────────────────────────────────────

export function useMachines() {
  const api = useApi();
  return useLiveQuery({ queryKey: keys.machines, queryFn: ({ signal }) => api.machines(signal) });
}

/** One task by id; idle while there is none. */
export function useTaskById(id: string | undefined) {
  const api = useApi();
  return useLiveQuery({
    queryKey: keys.tasks.detail(id ?? ''),
    queryFn: ({ signal }) => api.task(id ?? '', signal),
    enabled: id !== undefined,
  });
}

/** One workstream by id; idle while there is none. */
export function useWorkstreamById(id: string | undefined) {
  const api = useApi();
  return useLiveQuery({
    queryKey: keys.workstreams.detail(id ?? ''),
    queryFn: ({ signal }) => api.workstream(id ?? '', signal),
    enabled: id !== undefined,
  });
}

/** The facet filters of the session list. An empty list means "any". */
export interface SessionFacets {
  machine: string[];
  engine: Engine[];
  state: SessionState[];
  /** Project ids, or `UNSORTED` for sessions in no project. */
  project: string[];
  workstream: string[];
}

export const UNSORTED = 'unsorted';

export const NO_FACETS: SessionFacets = { machine: [], engine: [], state: [], project: [], workstream: [] };

/** What grouping and filtering need to know about where sessions belong. */
export interface SessionPlaces {
  projects: readonly Project[];
  workstreams: readonly Workstream[];
  tasks: readonly Task[];
}

/** The project and workstream a session belongs to: its workstream's, else its task's. */
export function placeOf(
  session: Session,
  places: SessionPlaces,
): { project: Project | undefined; workstream: Workstream | undefined } {
  const task = session.task === undefined ? undefined : places.tasks.find((t) => t.id === session.task);
  const workstreamId = session.workstream ?? task?.workstream;
  const workstream =
    workstreamId === undefined ? undefined : places.workstreams.find((w) => w.id === workstreamId);
  const projectId = workstream?.project ?? task?.project;
  const project = projectId === undefined ? undefined : places.projects.find((p) => p.id === projectId);
  return { project, workstream };
}

export function matchesFacets(session: Session, facets: SessionFacets, places: SessionPlaces): boolean {
  const any = <T>(values: readonly T[], value: T) => values.length === 0 || values.includes(value);
  if (!any(facets.machine, session.machine) || !any(facets.engine, session.engine) || !any(facets.state, session.state)) {
    return false;
  }
  if (facets.project.length === 0 && facets.workstream.length === 0) return true;
  const { project, workstream } = placeOf(session, places);
  return (
    any(facets.project, project?.id ?? UNSORTED) &&
    (facets.workstream.length === 0 || (workstream !== undefined && facets.workstream.includes(workstream.id)))
  );
}

/**
 * Every session, with what grouping needs, filtered by `facets` on the client. The list query is
 * the shared, live-patched `['sessions', 'list', {}]`.
 */
export function useConsoleSessions(facets: SessionFacets = NO_FACETS) {
  const sessions = useSessions();
  const projects = useProjects();
  const workstreams = useWorkstreams();
  const tasks = useTasks();
  const places: SessionPlaces = {
    projects: projects.data ?? [],
    workstreams: workstreams.data ?? [],
    tasks: tasks.data ?? [],
  };
  const all = sessions.data ?? [];
  const queries = [sessions, projects, workstreams, tasks];
  return {
    sessions: all.filter((s) => matchesFacets(s, facets, places)),
    all,
    places,
    isPending: queries.some((q) => q.isPending),
    error: queries.find((q) => q.error !== null)?.error ?? null,
  };
}

// ─── Transcript ─────────────────────────────────────────────────────────────────────────────────

interface WindowState {
  view: WindowView;
  /** The `before` of the page wanted next, if any. */
  wanted: number | undefined;
}

function createWindowStore() {
  const transcript = new TranscriptWindow();
  const store = createStore<WindowState>(() => ({ view: EMPTY_VIEW, wanted: undefined }));
  return {
    store,
    tail(page: TranscriptPage) {
      const consistent = transcript.addTail(page);
      store.setState((s) => ({ view: transcript.view, wanted: consistent ? s.wanted : undefined }));
    },
    older(before: number, page: TranscriptPage) {
      transcript.addOlder(before, page);
      store.setState({ view: transcript.view });
    },
    clear() {
      if (transcript.view === EMPTY_VIEW) return;
      transcript.clear();
      store.setState({ view: EMPTY_VIEW, wanted: undefined });
    },
    want(before: number | undefined) {
      if (store.getState().wanted !== before) store.setState({ wanted: before });
    },
  };
}

export interface TranscriptOptions {
  /** Items per page; the API's default (200) when absent. */
  pageSize?: number | undefined;
}

function tailKey(sessionId: string, limit: number | undefined) {
  const key = keys.sessions.transcript(sessionId);
  return limit === undefined ? key : [...key, { limit }];
}

function pageKey(sessionId: string, before: number, limit: number | undefined) {
  const key = keys.sessions.transcriptPage(sessionId, before);
  return limit === undefined ? key : [...key, { limit }];
}

/**
 * A session's transcript, tail first. The newest page is `keys.sessions.transcript(id)`, the only
 * one live events refetch; older pages (`keys.sessions.transcriptPage(id, before)`) load on
 * `loadOlder()` and are never refetched. Every page is merged into one window, so items stay put
 * while newer pages arrive; a gap between two ranges is fetched on its own.
 *
 * Key the caller by session id: the window belongs to one session.
 */
export function useTranscript(sessionId: string, options: TranscriptOptions = {}) {
  const api = useConsoleApi();
  const queryClient = useQueryClient();
  const limit = options.pageSize;
  const [slot] = useState(createWindowStore);
  const tail = useLiveQuery({
    queryKey: tailKey(sessionId, limit),
    queryFn: ({ signal }) => api.transcript(sessionId, { limit }, signal),
  });
  const wanted = useStore(slot.store, (s) => s.wanted);
  const view = useStore(slot.store, (s) => s.view);
  const older = useLiveQuery({
    queryKey: pageKey(sessionId, wanted ?? -1, limit),
    queryFn: ({ signal }) => api.transcript(sessionId, { before: wanted, limit }, signal),
    enabled: wanted !== undefined,
  });

  // Before paint, so the newest page shows in the frame it arrives in.
  useLayoutEffect(() => {
    if (tail.data !== undefined) slot.tail(tail.data);
    else slot.clear(); // the cache was reset (another event log): start over
  }, [tail.data, slot]);
  useLayoutEffect(() => {
    if (older.data !== undefined && wanted !== undefined) slot.older(wanted, older.data);
  }, [older.data, wanted, slot]);

  // A gap between the tail and older pages is filled without being asked.
  const olderBusy = older.isFetching;
  useEffect(() => {
    if (view.hasGap && !olderBusy) slot.want(view.nextBefore);
  }, [view, olderBusy, slot]);

  // The stream refetches the tail when a turn ends or a tool runs. A state change (a prompt was
  // sent, the agent started working) means the transcript moved too, so the tail follows it.
  const session = useSession(sessionId);
  const activity = session.data === undefined ? undefined : `${session.data.state}:${session.data.last_activity}`;
  const seen = useRef(activity);
  useEffect(() => {
    const previous = seen.current;
    seen.current = activity;
    if (previous !== undefined && activity !== undefined && previous !== activity) {
      void queryClient.invalidateQueries({ queryKey: tailKey(sessionId, limit), exact: true }, { cancelRefetch: false });
    }
  }, [activity, queryClient, sessionId, limit]);

  const loadingOlder = wanted !== undefined && older.isFetching;
  return {
    view,
    isPending: tail.isPending,
    error: tail.error ?? older.error,
    loadingOlder,
    /** Loads the page before the oldest loaded item, unless that is the start. */
    loadOlder() {
      if (!view.loaded || view.atStart || loadingOlder || view.nextBefore === undefined) return;
      slot.want(view.nextBefore);
    },
  };
}

// ─── Prompts sent from this window ──────────────────────────────────────────────────────────────

interface PendingState {
  bySession: Record<string, PendingPrompt[]>;
  add(session: string, text: string): string;
  remove(session: string, ids: ReadonlySet<string>): void;
}

let pendingId = 0;

/** Prompts sent but not yet in the transcript; the chat shows them at its end meanwhile. */
export const usePendingPrompts = create<PendingState>()((set) => ({
  bySession: {},
  add(session, text) {
    pendingId += 1;
    const id = `p${pendingId}`;
    set((s) => ({
      bySession: {
        ...s.bySession,
        [session]: [...(s.bySession[session] ?? []), { id, text, sentAt: Date.now() }],
      },
    }));
    return id;
  },
  remove(session, ids) {
    set((s) => ({
      bySession: { ...s.bySession, [session]: (s.bySession[session] ?? []).filter((p) => !ids.has(p.id)) },
    }));
  },
}));

const NONE: PendingPrompt[] = [];

/** This session's pending prompts, dropping those the transcript now shows. */
export function usePending(sessionId: string, view: WindowView): PendingPrompt[] {
  const pending = usePendingPrompts((s) => s.bySession[sessionId] ?? NONE);
  const remove = usePendingPrompts((s) => s.remove);
  const delivered = deliveredPrompts(pending, view.items);
  useEffect(() => {
    if (delivered.size > 0) remove(sessionId, delivered);
  }, [delivered, remove, sessionId]);
  return delivered.size === 0 ? pending : pending.filter((p) => !delivered.has(p.id));
}

// ─── Writes ─────────────────────────────────────────────────────────────────────────────────────

/** Types text into the session and presses Enter. The chat shows it until the transcript does. */
export function useSendText(sessionId: string) {
  const api = useConsoleApi();
  const add = usePendingPrompts((s) => s.add);
  const remove = usePendingPrompts((s) => s.remove);
  return useMutation({
    mutationFn: (text: string) => api.send(sessionId, text),
    onMutate: (text) => ({ pending: add(sessionId, text) }),
    onError: (_error, _text, context) => {
      if (context !== undefined) remove(sessionId, new Set([context.pending]));
    },
  });
}

export function useSendKeys(sessionId: string) {
  const api = useConsoleApi();
  return useMutation({ mutationFn: (keys: readonly Key[]) => api.keys(sessionId, keys) });
}

export function useInterrupt(sessionId: string) {
  const api = useConsoleApi();
  return useMutation({ mutationFn: () => api.interrupt(sessionId) });
}

export function useEndSession(sessionId: string) {
  const api = useConsoleApi();
  return useMutation({ mutationFn: (mode: EndMode) => api.end(sessionId, mode) });
}

/** Answers an ask with an option, a text, or both. `ask_answered` then refreshes the asks. */
export function useAnswerAsk() {
  const api = useConsoleApi();
  return useMutation({
    mutationFn: ({ ask, option, text }: { ask: string; option?: number | undefined; text?: string | undefined }) =>
      api.answerAsk(ask, {
        ...(option === undefined ? {} : { option }),
        ...(text === undefined ? {} : { text }),
      }),
  });
}
