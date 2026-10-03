// Connects the delta stream to the TanStack Query cache.
//
// - The stream starts first. Data queries wait for its first `hello` (`synced`), so every fetch
//   reflects at least that revision and every later change arrives as an event.
// - Events that carry whole objects are patched in; the rest invalidate their keys, coalesced over
//   a short window and without cancelling fetches in flight. A query whose fetch is in flight when
//   an event touches it is refetched once, by its exact key, after that fetch settles, since the
//   fetch may predate the event. The same goes for a patch that lands while a fetch under its keys
//   is in flight: the older response would overwrite it.
// - A reset (a replaced event log) drops the cache; coming back from `reconnecting`
//   refetches failed queries.

import type { QueryClient } from '@tanstack/react-query';
import { createStore, type StoreApi } from 'zustand/vanilla';
import { ApiError, GatewayError } from './errors.ts';
import { keysToInvalidate, type CacheLookup, type QueryKey } from './invalidation.ts';
import { keys } from './keys.ts';
import { applyPatches } from './patches.ts';
import { recapScopeForEvents, type RecapCacheLookup, type RecapScope } from './recaps.ts';
import { StreamClient, type StreamStatus } from './stream.ts';
import type { Transport } from './transport.ts';
import type { Ask, Dispatch, RecapBlockFilters, RecapDayScope, Session, Task, Workstream } from './types.ts';

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

/** Finds `id` in a cached detail, else in any cached list under `prefix`. */
function findCached<T extends { id: string }>(queryClient: QueryClient, detail: QueryKey, prefix: QueryKey, id: string): T | undefined {
  const found = queryClient.getQueryData<T>(detail);
  if (found !== undefined) return found;
  for (const [, list] of queryClient.getQueriesData<T[]>({ queryKey: prefix })) {
    const item = Array.isArray(list) ? list.find((x) => x.id === id) : undefined;
    if (item !== undefined) return item;
  }
  return undefined;
}

/**
 * `RecapCacheLookup` from the query cache: `undefined` when the entity itself is not cached (its
 * parents are then unknown), an object when it is — even if some of its own fields are absent.
 * There is no cached list of dispatches today (nothing in `src/data` fetches one), so
 * `dispatch_started`/`dispatch_finished` usually fall back to invalidating every recap key; that
 * is the contract's documented, always-correct fallback.
 */
export function recapCacheLookup(queryClient: QueryClient): RecapCacheLookup {
  return {
    session(id) {
      const found = findCached<Session>(queryClient, keys.sessions.detail(id), keys.sessions.lists, id);
      return found && { task: found.task, workstream: found.workstream };
    },
    task(id) {
      const found = findCached<Task>(queryClient, keys.tasks.detail(id), keys.tasks.lists, id);
      return found && { workstream: found.workstream, project: found.project };
    },
    workstream(id) {
      const found = findCached<Workstream>(queryClient, keys.workstreams.detail(id), keys.workstreams.lists, id);
      return found && { project: found.project };
    },
    dispatch(id) {
      for (const [, dispatches] of queryClient.getQueriesData<Dispatch[]>({ queryKey: keys.dispatches })) {
        const found = Array.isArray(dispatches) ? dispatches.find((d) => d.id === id) : undefined;
        if (found !== undefined) return { task: found.task, session: found.session };
      }
      return undefined;
    },
    ask(id) {
      for (const [, asks] of queryClient.getQueriesData<Ask[]>({ queryKey: keys.asks.lists })) {
        const found = Array.isArray(asks) ? asks.find((a) => a.id === id) : undefined;
        if (found !== undefined) return { task: found.task, session: found.session };
      }
      return undefined;
    },
  };
}

function matchesBlockFilters(filters: RecapBlockFilters, scope: RecapScope): boolean {
  return (
    (filters.session !== undefined && scope.session.includes(filters.session)) ||
    (filters.task !== undefined && scope.task.includes(filters.task)) ||
    (filters.workstream !== undefined && scope.workstream.includes(filters.workstream)) ||
    (filters.project !== undefined && scope.project.includes(filters.project))
  );
}

function matchesDayScope(dayScope: RecapDayScope, scope: RecapScope): boolean {
  return 'workstream' in dayScope ? scope.workstream.includes(dayScope.workstream) : scope.project.includes(dayScope.project);
}

/** Recap keys to invalidate, split by how: see `recapKeysForScope`. */
export interface RecapKeys {
  /** Invalidated as prefixes (`Invalidator.add`): each is itself a specific, non-empty filter or
   * scope, so matching its own longer (e.g. `{limit}`-suffixed) variants too is what we want. */
  prefix: QueryKey[];
  /** Invalidated by their own exact key only (`Invalidator.addExact`): queries actually mounted
   * with empty blocks filters (`{}`), found by scanning the cache rather than guessing a key,
   * because TanStack matches `{}` against *any* object — "prefix" matching it would invalidate
   * every blocks query, filtered or not (see `Invalidator.#flush`'s own note on this). */
  exact: QueryKey[];
}

/**
 * The recap keys a batch's scope touches (API v1, "Recaps", "Live updates"): nothing for
 * `undefined` (no event in the batch was activity), every `['recaps']` key for `'everything'`
 * (an unresolved link, `member_added`, or an unknown event type), else the unfiltered blocks key
 * plus every mounted recap query (blocks or days, whatever `filters`/`limit` it was given) whose
 * own filter value is in scope.
 */
export function recapKeysForScope(scope: RecapScope | 'everything' | undefined, queryClient: QueryClient): RecapKeys {
  if (scope === undefined) return { prefix: [], exact: [] };
  if (scope === 'everything') return { prefix: [keys.recaps.all], exact: [] };
  const prefix: QueryKey[] = [];
  const exact: QueryKey[] = [];
  for (const query of queryClient.getQueryCache().findAll({ queryKey: keys.recaps.all })) {
    const [, kind, filtersOrScope] = query.queryKey as [string, string, unknown];
    if (kind === 'blocks' && Object.keys((filtersOrScope as object | undefined) ?? {}).length === 0) {
      exact.push(query.queryKey as QueryKey);
    } else if (kind === 'blocks' && matchesBlockFilters((filtersOrScope ?? {}) as RecapBlockFilters, scope)) {
      prefix.push(query.queryKey as QueryKey);
    } else if (kind === 'days' && matchesDayScope(filtersOrScope as RecapDayScope, scope)) {
      prefix.push(query.queryKey as QueryKey);
    }
  }
  return { prefix, exact };
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
  /** Keys to invalidate by their exact key only, never as a prefix (see `addExact`). */
  readonly #pendingExact = new Map<string, QueryKey>();
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
      if (key.length === 0) {
        this.#everythingPending = true;
        continue;
      }
      this.#awaitFetches(key);
      this.#pending.set(JSON.stringify(key), key);
    }
    this.#schedule();
    this.#scheduleEverything();
  }

  /**
   * Invalidates each of `keys` by its own exact key only, never as a prefix: for a key whose last
   * segment is `{}` (e.g. a blocks query with no filters), TanStack matches that against *any*
   * object, so invalidating it as a prefix would reach every differently-filtered query too.
   * Coalesced like `add()`, but flushed with `exact: true`.
   */
  addExact(keys: readonly QueryKey[]): void {
    for (const key of keys) {
      this.#awaitFetches(key);
      this.#pendingExact.set(JSON.stringify(key), key);
    }
    this.#schedule();
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
    this.#pendingExact.clear();
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
    if (
      this.#timer === undefined &&
      (this.#pending.size > 0 || this.#settled.size > 0 || this.#pendingExact.size > 0)
    ) {
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
      this.#awaitFetches([]);
      this.#invalidate({});
    }, wait);
  }

  #flush(): void {
    this.#timer = undefined;
    const pending = [...this.#pending.values()];
    const settled = [...this.#settled.values()];
    const pendingExact = [...this.#pendingExact.values()];
    this.#pending.clear();
    this.#settled.clear();
    this.#pendingExact.clear();
    for (const queryKey of pending) this.#invalidate({ queryKey });
    // Exact: `['tasks', 'list', {}]` is also a prefix of every filtered task list.
    for (const queryKey of settled) this.#invalidate({ queryKey, exact: true });
    for (const queryKey of pendingExact) this.#invalidate({ queryKey, exact: true });
  }

  /** Invalidates without cancelling, and spares queries still waiting for a racing fetch. */
  #invalidate(filters: { queryKey?: QueryKey; exact?: boolean }): void {
    void this.#queryClient.invalidateQueries(
      { ...filters, predicate: (query) => !this.#waiting.has(query.queryHash) },
      { cancelRefetch: false },
    );
  }
}

/**
 * Why the stream keeps failing, once we know. `needs_pairing` is the desktop gateway's: it has no
 * valid token for the workspace.
 */
export type LiveProblem = 'unauthorized' | 'unreachable' | 'needs_pairing';

export interface LiveState {
  status: StreamStatus;
  /** The stream has said `hello` at least once; data queries may run. */
  synced: boolean;
  problem?: LiveProblem | undefined;
}

export interface LiveOptions {
  queryClient: QueryClient;
  /** Where the stream's socket comes from. */
  transport: Transport;
  /** The coalescing window for invalidations. */
  windowMs?: number;
  backoff?: { initialMs: number; maxMs: number };
  /**
   * An authenticated request (e.g. `GET /v1/me`), tried after repeated connection failures to
   * tell a rejected token from an unreachable hub; a browser WebSocket failure does not say
   * which. Not needed when the socket says why it failed (the desktop gateway does).
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
  /** The stream's transport; terminals open their sockets through it too. */
  transport: Transport;
  start(): void;
  stop(): void;
  /** Reconnects now if the stream is waiting to (the hub is known to be back). */
  retryNow(): void;
}

function problemOf(error: unknown): LiveProblem {
  if (error instanceof GatewayError && error.gateway === 'needs_pairing') return 'needs_pairing';
  if (error instanceof ApiError && (error.code === 'unauthorized' || error.code === 'forbidden')) {
    return 'unauthorized';
  }
  return 'unreachable';
}

export function createLive(options: LiveOptions): Live {
  const { queryClient, transport } = options;
  const cache = cacheLookup(queryClient);
  const recapCache = recapCacheLookup(queryClient);
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

  /** Finds out why the stream fails: from `known` (what the socket said), or by probing. */
  async function diagnose(known: ApiError | undefined): Promise<void> {
    if (probing) return;
    probing = true;
    // Without a probe, all we know is that the stream cannot connect.
    let problem: LiveProblem | undefined = 'unreachable';
    let cause: unknown = known;
    try {
      if (known !== undefined) {
        problem = problemOf(known);
      } else if (options.probe !== undefined) {
        await options.probe();
        // The hub answers and accepts the token; only the stream fails.
        problem = undefined;
      }
    } catch (error) {
      cause = error;
      problem = problemOf(error);
    } finally {
      probing = false;
    }
    if (store.getState().status !== 'reconnecting') return;
    store.setState({ problem });
    // The desktop gateway says why, for people to read.
    const why = cause instanceof GatewayError && cause.message !== '' ? ` (${cause.message})` : '';
    const warning =
      problem === 'unauthorized'
        ? `pitcrew: ${transport.label} rejected the token${why}; the stream keeps retrying.`
        : problem === 'needs_pairing'
          ? `pitcrew: ${transport.label} needs pairing${why}; the stream keeps retrying.`
          : problem === 'unreachable'
            ? `pitcrew: cannot reach ${transport.label}${why}; the stream keeps retrying.`
            : `pitcrew: ${transport.label} answers, but its stream keeps failing; retrying.`;
    if (warning !== warned) {
      warned = warning;
      console.warn(warning);
    }
  }

  const stream = new StreamClient({
    transport,
    ...(options.backoff === undefined ? {} : { backoff: options.backoff }),
    onEvents(events) {
      try {
        const { touched, failed } = applyPatches(queryClient, events);
        invalidator.afterFetches(touched);
        const recapKeys = recapKeysForScope(recapScopeForEvents(events, recapCache), queryClient);
        invalidator.add([...keysToInvalidate(events, cache), ...recapKeys.prefix, ...failed]);
        invalidator.addExact(recapKeys.exact);
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
    onFailure(attempts, close) {
      // `attempts` starts over only after a stable connection, so this counts within an outage.
      const since = attempts - probeAfter;
      if (since === 0 || (since > 0 && since % probeEvery === 0)) void diagnose(close?.error);
    },
  });

  return {
    store,
    stream,
    transport,
    start: () => stream.start(),
    stop() {
      stream.stop();
      invalidator.stop();
    },
    retryNow: () => stream.retryNow(),
  };
}
