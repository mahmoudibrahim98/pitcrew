// Connects the delta stream to the TanStack Query cache.
//
// - The stream starts first. Data queries wait for its first `hello` (`synced`), so every fetch
//   reflects at least that revision and every later change arrives as an event.
// - Events that carry whole objects are patched in; the rest invalidate their keys, coalesced over
//   a short window and without cancelling fetches in flight. A key whose query is mid-fetch is
//   retried after the fetch, since that fetch may predate the event.
// - A reset (or a gap) drops the cache; coming back from `reconnecting` refetches failed queries.

import type { QueryClient } from '@tanstack/react-query';
import { createStore, type StoreApi } from 'zustand/vanilla';
import { keysToInvalidate, type CacheLookup, type QueryKey } from './invalidation.ts';
import { keys } from './keys.ts';
import { applyPatches } from './patches.ts';
import { StreamClient, type SocketFactory, type StreamStatus } from './stream.ts';
import type { Task } from './types.ts';

export function cacheLookup(queryClient: QueryClient): CacheLookup {
  return {
    taskWorkstream(id) {
      const detail = queryClient.getQueryData<Task>(keys.tasks.detail(id));
      if (detail !== undefined) return detail.workstream;
      for (const [, tasks] of queryClient.getQueriesData<Task[]>({ queryKey: keys.tasks.lists })) {
        const found = tasks?.find((task) => task.id === id);
        if (found !== undefined) return found.workstream;
      }
      return undefined;
    },
  };
}

/** Coalesces invalidations per key and flushes them at most once per `windowMs`. */
export class Invalidator {
  readonly #queryClient: QueryClient;
  readonly #windowMs: number;
  readonly #everythingMs: number;
  readonly #pending = new Map<string, QueryKey>();
  #timer: ReturnType<typeof setTimeout> | undefined;
  #lastEverything = -Infinity;

  constructor(queryClient: QueryClient, options: { windowMs?: number; everythingMs?: number } = {}) {
    this.#queryClient = queryClient;
    this.#windowMs = options.windowMs ?? 250;
    this.#everythingMs = options.everythingMs ?? 5_000;
  }

  add(keys: readonly QueryKey[]): void {
    for (const key of keys) this.#pending.set(JSON.stringify(key), key);
    this.#schedule(this.#windowMs);
  }

  stop(): void {
    if (this.#timer !== undefined) clearTimeout(this.#timer);
    this.#timer = undefined;
    this.#pending.clear();
  }

  #schedule(delay: number): void {
    if (this.#timer === undefined && this.#pending.size > 0) {
      this.#timer = setTimeout(() => this.#flush(), delay);
    }
  }

  #flush(): void {
    this.#timer = undefined;
    const now = Date.now();
    const cache = this.#queryClient.getQueryCache();
    const pending = [...this.#pending.entries()];
    this.#pending.clear();
    let wait = this.#windowMs;
    for (const [id, queryKey] of pending) {
      // "Everything" (an unknown event type) is rate-limited.
      if (queryKey.length === 0 && now - this.#lastEverything < this.#everythingMs) {
        this.#pending.set(id, queryKey);
        wait = Math.max(wait, this.#lastEverything + this.#everythingMs - now);
        continue;
      }
      if (queryKey.length === 0) this.#lastEverything = now;
      const busy = cache.findAll({ queryKey, fetchStatus: 'fetching' }).length > 0;
      if (busy) this.#pending.set(id, queryKey);
      void this.#queryClient.invalidateQueries(
        { queryKey, predicate: (query) => query.state.fetchStatus !== 'fetching' },
        { cancelRefetch: false },
      );
    }
    this.#schedule(wait);
  }
}

export interface LiveState {
  status: StreamStatus;
  /** The stream has said `hello` at least once; data queries may run. */
  synced: boolean;
}

export interface LiveOptions {
  queryClient: QueryClient;
  baseUrl: string;
  token?: string | undefined;
  socket?: SocketFactory;
  /** The coalescing window for invalidations. */
  windowMs?: number;
  backoff?: { initialMs: number; maxMs: number };
}

export interface Live {
  store: StoreApi<LiveState>;
  stream: StreamClient;
  start(): void;
  stop(): void;
}

export function createLive(options: LiveOptions): Live {
  const { queryClient } = options;
  const cache = cacheLookup(queryClient);
  const store = createStore<LiveState>(() => ({ status: 'stopped', synced: false }));
  const invalidator = new Invalidator(
    queryClient,
    options.windowMs === undefined ? {} : { windowMs: options.windowMs },
  );

  const stream = new StreamClient({
    baseUrl: options.baseUrl,
    token: options.token,
    ...(options.socket === undefined ? {} : { socket: options.socket }),
    ...(options.backoff === undefined ? {} : { backoff: options.backoff }),
    onEvents(events) {
      applyPatches(queryClient, events);
      invalidator.add(keysToInvalidate(events, cache));
    },
    onReset() {
      invalidator.stop();
      void queryClient.resetQueries();
    },
    onStatus(status) {
      const was = store.getState();
      store.setState({ status, synced: was.synced || status === 'live' });
      if (status === 'live' && was.status === 'reconnecting') {
        void queryClient.refetchQueries({ predicate: (query) => query.state.status === 'error' });
      }
    },
  });

  return {
    store,
    stream,
    start: () => stream.start(),
    stop() {
      stream.stop();
      invalidator.stop();
    },
  };
}
