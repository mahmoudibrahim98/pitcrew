// Connects the delta stream to the TanStack Query cache.
//
// - The stream starts first. Data queries wait for its first `hello` (`synced`), so every fetch
//   reflects at least that revision and every later change arrives as an event.
// - Events that carry whole objects are patched in; the rest invalidate their keys, coalesced over
//   a short window and without cancelling fetches in flight. A query whose fetch is in flight when
//   an event touches it is refetched once, by its exact key, after that fetch settles, since the
//   fetch may predate the event. The same goes for a patch that lands while a fetch under its keys
//   is in flight: the older response would overwrite it.
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
 * Invalidates query keys for the stream, coalesced per key and flushed at most once per
 * `windowMs`, without cancelling fetches in flight.
 *
 * A query whose fetch is in flight when its key is added may get an answer from before the event,
 * so it is left alone at the flush and refetched once, by its exact key, after that fetch settles.
 * Its idle siblings under the same key are invalidated once, at the flush. Each event therefore
 * costs every query at most one refetch. Invalidating everything (an unknown event type) is
 * rate-limited on its own timer.
 */
export class Invalidator {
  readonly #queryClient: QueryClient;
  readonly #windowMs: number;
  readonly #everythingMs: number;
  /** Keys (prefixes) to invalidate at the next flush. */
  readonly #pending = new Map<string, QueryKey>();
  /** Hashes of queries whose fetch was in flight when an event touched them. */
  readonly #waiting = new Set<string>();
  /** Exact keys of queries whose racing fetch has settled, to invalidate at the next flush. */
  readonly #settled = new Map<string, QueryKey>();
  /** Bumped by `stop()`, so fetches that settle afterwards are ignored. */
  #generation = 0;
  #timer: ReturnType<typeof setTimeout> | undefined;
  #everythingTimer: ReturnType<typeof setTimeout> | undefined;
  #everythingPending = false;
  #lastEverything = -Infinity;

  constructor(queryClient: QueryClient, options: { windowMs?: number; everythingMs?: number } = {}) {
    this.#queryClient = queryClient;
    this.#windowMs = options.windowMs ?? 250;
    this.#everythingMs = options.everythingMs ?? 5_000;
  }

  /** Invalidates every query under `keys`; the key `[]` means everything. */
  add(keys: readonly QueryKey[]): void {
    for (const key of keys) {
      this.#awaitFetches(key);
      if (key.length === 0) this.#everythingPending = true;
      else this.#pending.set(JSON.stringify(key), key);
    }
    this.#schedule();
    this.#scheduleEverything();
  }

  /**
   * For keys the cache was just written under: refetches the queries among them whose fetch is
   * in flight, after it settles, since its older answer will overwrite the write.
   */
  afterFetches(keys: readonly QueryKey[]): void {
    for (const key of keys) this.#awaitFetches(key);
  }

  stop(): void {
    if (this.#timer !== undefined) clearTimeout(this.#timer);
    if (this.#everythingTimer !== undefined) clearTimeout(this.#everythingTimer);
    this.#timer = undefined;
    this.#everythingTimer = undefined;
    this.#pending.clear();
    this.#waiting.clear();
    this.#settled.clear();
    this.#everythingPending = false;
    this.#generation += 1;
  }

  /** Waits for each fetch in flight under `queryKey`, then queues its query's exact key. */
  #awaitFetches(queryKey: QueryKey): void {
    const generation = this.#generation;
    for (const query of this.#queryClient.getQueryCache().findAll({ queryKey, fetchStatus: 'fetching' })) {
      const { queryHash } = query;
      if (this.#waiting.has(queryHash)) continue;
      const settle = () => {
        if (generation !== this.#generation) return;
        this.#waiting.delete(queryHash);
        this.#settled.set(queryHash, query.queryKey);
        this.#schedule();
      };
      const promise = query.promise;
      if (promise === undefined) {
        settle();
        continue;
      }
      this.#waiting.add(queryHash);
      void promise.then(settle, settle);
    }
  }

  #schedule(): void {
    if (this.#timer === undefined && (this.#pending.size > 0 || this.#settled.size > 0)) {
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
      this.#invalidate({});
    }, wait);
  }

  #flush(): void {
    this.#timer = undefined;
    const pending = [...this.#pending.values()];
    const settled = [...this.#settled.values()];
    this.#pending.clear();
    this.#settled.clear();
    for (const queryKey of pending) this.#invalidate({ queryKey });
    // Exact: `['tasks', 'list', {}]` is also a prefix of every filtered task list.
    for (const queryKey of settled) this.#invalidate({ queryKey, exact: true });
  }

  /** Invalidates without cancelling, and spares queries still waiting for a racing fetch. */
  #invalidate(filters: { queryKey?: QueryKey; exact?: boolean }): void {
    void this.#queryClient.invalidateQueries(
      { ...filters, predicate: (query) => !this.#waiting.has(query.queryHash) },
      { cancelRefetch: false },
    );
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
  /** Failures before the first probe. */
  probeAfter?: number;
  /** Failures between later probes in the same outage, in case the reason changes. */
  probeEvery?: number;
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
  const probeEvery = options.probeEvery ?? 5;
  let probing = false;
  /** The last warning logged in this outage; each reason is logged once. */
  let warned: string | undefined;

  async function diagnose(): Promise<void> {
    if (probing) return;
    probing = true;
    // Without a probe, all we know is that the stream cannot connect.
    let problem: LiveProblem | undefined = 'unreachable';
    try {
      if (options.probe !== undefined) {
        await options.probe();
        // The hub answers and accepts the token; only the stream fails.
        problem = undefined;
      }
    } catch (error) {
      problem =
        error instanceof ApiError && (error.code === 'unauthorized' || error.code === 'forbidden')
          ? 'unauthorized'
          : 'unreachable';
    } finally {
      probing = false;
    }
    if (store.getState().status !== 'reconnecting') return;
    store.setState({ problem });
    const warning =
      problem === 'unauthorized'
        ? `pitcrew: ${options.baseUrl} rejected the token; the stream keeps retrying.`
        : problem === 'unreachable'
          ? `pitcrew: cannot reach ${options.baseUrl}; the stream keeps retrying.`
          : `pitcrew: ${options.baseUrl} answers, but its stream keeps failing; retrying.`;
    if (warning !== warned) {
      warned = warning;
      console.warn(warning);
    }
  }

  const stream = new StreamClient({
    baseUrl: options.baseUrl,
    token: options.token,
    ...(options.socket === undefined ? {} : { socket: options.socket }),
    ...(options.backoff === undefined ? {} : { backoff: options.backoff }),
    onEvents(events) {
      try {
        const { touched, failed } = applyPatches(queryClient, events);
        invalidator.afterFetches(touched);
        invalidator.add([...keysToInvalidate(events, cache), ...failed]);
      } catch (error) {
        // A malformed event. `rev` has moved past the batch, so refetch rather than lose it.
        console.warn('pitcrew: could not apply an event batch; refetching everything', error);
        invalidator.add([[]]);
      }
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
      if (status === 'live') warned = undefined;
      if (status === 'live' && was.status === 'reconnecting') {
        void queryClient.refetchQueries({
          type: 'active',
          predicate: (query) => query.state.status === 'error',
        });
      }
    },
    onFailure(attempts) {
      // `attempts` starts over only after a stable connection, so this counts within an outage.
      const since = attempts - probeAfter;
      if (since === 0 || (since > 0 && since % probeEvery === 0)) void diagnose();
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
