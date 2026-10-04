// The console's data hooks, on the data layer's `useLiveQuery` and keys (src/data/README.md).
// Lists and details reuse the shared keys, so the stream's patches and invalidations reach them.
// Mutations never touch the cache: the events they cause do.

import { useMutation } from '@tanstack/react-query';
import { useEffect, useLayoutEffect, useState } from 'react';
import { create, useStore } from 'zustand';
import { createStore } from 'zustand/vanilla';
import {
  keys,
  useApi,
  useLiveQuery,
  useProjects,
  useSessions,
  useTasks,
  useWorkstreams,
  type EndMode,
  type Key,
  type TranscriptPage,
} from '../data/index.ts';
import { matchesFacets, NO_FACETS, type SessionFacets, type SessionPlaces } from './facets.ts';
import { useNow } from './format.ts';
import { deliveredPrompts, EMPTY_VIEW, TranscriptWindow, type PendingPrompt, type WindowView } from './transcript.ts';

// ─── Reads ──────────────────────────────────────────────────────────────────────────────────────

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

/** The workspace's personas (an agent's model comes from its persona). */
export function usePersonas() {
  const api = useApi();
  return useLiveQuery({ queryKey: keys.personas, queryFn: ({ signal }) => api.personas(signal) });
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
  const api = useApi();
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

/** How long a sent prompt shows before the transcript has it; after that it is dropped. */
const PENDING_FOR = 120_000;

/** This session's pending prompts, dropping those the transcript now shows and stale ones. */
export function usePending(sessionId: string, view: WindowView): PendingPrompt[] {
  const pending = usePendingPrompts((s) => s.bySession[sessionId] ?? NONE);
  const remove = usePendingPrompts((s) => s.remove);
  const now = useNow();
  const done = deliveredPrompts(pending, view.items);
  for (const prompt of pending) {
    if (now - prompt.sentAt > PENDING_FOR) done.add(prompt.id);
  }
  useEffect(() => {
    if (done.size > 0) remove(sessionId, done);
  }, [done, remove, sessionId]);
  return done.size === 0 ? pending : pending.filter((p) => !done.has(p.id));
}

// ─── Writes ─────────────────────────────────────────────────────────────────────────────────────

/** Types text into the session and presses Enter. The chat shows it until the transcript does. */
export function useSendText(sessionId: string) {
  const api = useApi();
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
  const api = useApi();
  return useMutation({ mutationFn: (keys: readonly Key[]) => api.keys(sessionId, keys) });
}

export function useInterrupt(sessionId: string) {
  const api = useApi();
  return useMutation({ mutationFn: () => api.interrupt(sessionId) });
}

export function useEndSession(sessionId: string) {
  const api = useApi();
  return useMutation({ mutationFn: (mode: EndMode) => api.end(sessionId, mode) });
}

/** Answers an ask with an option, a text, or both. `ask_answered` then refreshes the asks. */
export function useAnswerAsk() {
  const api = useApi();
  return useMutation({
    mutationFn: ({ ask, option, text }: { ask: string; option?: number | undefined; text?: string | undefined }) =>
      api.answerAsk(ask, {
        ...(option === undefined ? {} : { option }),
        ...(text === undefined ? {} : { text }),
      }),
  });
}
