// Connects the delta stream to the TanStack Query cache.
//
// - The stream starts first. Data queries wait for its first `hello` (`synced`), so every fetch
//   reflects at least that revision and every later change arrives as an event.
// - Events that carry whole objects are patched in; the rest invalidate their keys, coalesced over
//   a short window and without cancelling fetches in flight. A key whose query is mid-fetch is
//   retried after the fetch, since that fetch may predate the event. The same goes for a patch
//   that lands while a fetch under its keys is in flight: the older response would overwrite it.
// - A reset (a gap, or another event log) drops the cache; coming back from `reconnecting`
//   refetches failed queries.

import type { QueryClient } from '@tanstack/react-query';
import { createStore, type StoreApi } from 'zustand/vanilla';
import { ApiError } from './api.ts';
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
        const found = Array.isArray(tasks) ? tasks.find((task) => task.id === id) : undefined;
        if (found !== undefined) return found.workstream;
      }
      return undefined;
    },
  };
}

/**
 * Coalesces invalidations per key and flushes them at most once per `windowMs`. Invalidating
 * everything (an unknown event type) is rate-limited on its own timer.
 */
export class Invalidator {
  readonly #queryClient: QueryClient;
  readonly #windowMs: number;
  readonly #everythingMs: number;
  readonly #pending = new Map<string, QueryKey>();
  #timer: ReturnType<typeof setTimeout> | undefined;
  #everythingTimer: ReturnType<typeof setTimeout> | undefined;
  #everythingPending = false;
  #lastEverything = -Infinity;

  constructor(queryClient: QueryClient, options: { windowMs?: number; everythingMs?: number } = {}) {
    this.#queryClient = queryClient;
    this.#windowMs = options.windowMs ?? 250;
    this.#everythingMs = options.everythingMs ?? 5_000;
  }

  add(keys: readonly QueryKey[]): void {
    for (const key of keys) {
      if (key.length === 0) this.#everythingPending = true;
      else this.#pending.set(JSON.stringify(key), key);
    }
    this.#schedule();
    this.#scheduleEverything();
  }

  stop(): void {
    if (this.#timer !== undefined) clearTimeout(this.#timer);
    if (this.#everythingTimer !== undefined) clearTimeout(this.#everythingTimer);
    this.#timer = undefined;
    this.#everythingTimer = undefined;
    this.#pending.clear();
    this.#everythingPending = false;
  }

  #schedule(): void {
    if (this.#timer === undefined && this.#pending.size > 0) {
      this.#timer = setTimeout(() => this.#flush(), this.#windowMs);
    }
  }

  #scheduleEverything(): void {
    if (this.#everythingTimer !== undefined || !this.#everythingPending) return;
    const wait = Math.max(this.#windowMs, this.#lastEverything + this.#everythingMs - Date.now());
    this.#everythingTimer = setTimeout(() => {
      this.#everythingTimer = undefined;
      this.#everythingPending = false;
      this.#lastEverything = Date.now();
      void this.#queryClient.invalidateQueries(undefined, { cancelRefetch: false });
    }, wait);
  }

  #flush(): void {
    this.#timer = undefined;
    const cache = this.#queryClient.getQueryCache();
    const pending = [...this.#pending.entries()];
    this.#pending.clear();
    for (const [id, queryKey] of pending) {
      if (cache.findAll({ queryKey, fetchStatus: 'fetching' }).length > 0) {
        this.#pending.set(id, queryKey);
      }
      void this.#queryClient.invalidateQueries(
        { queryKey, predicate: (query) => query.state.fetchStatus !== 'fetching' },
        { cancelRefetch: false },
      );
    }
    this.#schedule();
  }
}

/** Why the stream keeps failing, once we know. */
export type LiveProblem = 'unauthorized' | 'unreachable';

export interface LiveState {
  status: StreamStatus;
  /** The stream has said `hello` at least once; data queries may run. */
  synced: boolean;
  problem?: LiveProblem | undefined;
}

export interface LiveOptions {
  queryClient: QueryClient;
  baseUrl: string;
  token?: string | undefined;
  socket?: SocketFactory;
  /** The coalescing window for invalidations. */
  windowMs?: number;
  backoff?: { initialMs: number; maxMs: number };
  /**
   * An authenticated HTTP request (e.g. `GET /v1/me`), tried after repeated connection failures
   * to tell a rejected token from an unreachable hub; a WebSocket failure does not say which.
   */
  probe?: () => Promise<unknown>;
  /** Failures before probing. */
  probeAfter?: number;
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
  const probeAfter = options.probeAfter ?? 3;

  async function diagnose(): Promise<void> {
    let problem: LiveProblem = 'unreachable';
    try {
      await options.probe?.();
    } catch (error) {
      if (error instanceof ApiError && (error.code === 'unauthorized' || error.code === 'forbidden')) {
        problem = 'unauthorized';
      }
    }
    if (store.getState().status === 'live') return;
    store.setState({ problem });
    console.warn(
      problem === 'unauthorized'
        ? `pitcrew: ${options.baseUrl} rejected the token; the stream keeps retrying.`
        : `pitcrew: cannot reach ${options.baseUrl}; the stream keeps retrying.`,
    );
  }

  const stream = new StreamClient({
    baseUrl: options.baseUrl,
    token: options.token,
    ...(options.socket === undefined ? {} : { socket: options.socket }),
    ...(options.backoff === undefined ? {} : { backoff: options.backoff }),
    onEvents(events) {
      const { touched, failed } = applyPatches(queryClient, events);
      const racing = touched.filter((queryKey) => queryClient.isFetching({ queryKey }) > 0);
      invalidator.add([...keysToInvalidate(events, cache), ...failed, ...racing]);
    },
    onReset() {
      invalidator.stop();
      void queryClient.resetQueries();
    },
    onStatus(status) {
      const was = store.getState();
      store.setState({
        status,
        synced: was.synced || status === 'live',
        problem: status === 'live' ? undefined : was.problem,
      });
      if (status === 'live' && was.status === 'reconnecting') {
        void queryClient.refetchQueries({
          type: 'active',
          predicate: (query) => query.state.status === 'error',
        });
      }
    },
    onFailure(attempts) {
      // Once per outage: the count starts over only after a stable connection.
      if (attempts === probeAfter) void diagnose();
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
