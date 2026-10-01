// The impure half of recap invalidation: reading the query cache (`recapCacheLookup`) and turning
// a scope into the keys to invalidate (`recapKeysForScope`), plus end-to-end coverage of `live.ts`
// wiring them to real and scripted event streams. The event -> scope rules themselves are
// table-tested, without a QueryClient, in `recaps.test.ts`.

import { QueryClient, QueryObserver } from '@tanstack/react-query';
import { afterEach, describe, expect, it, vi } from 'vitest';
import { createApi, type Api } from '../src/data/api.ts';
import { keys } from '../src/data/keys.ts';
import { createLive, recapCacheLookup, recapKeysForScope, type Live } from '../src/data/live.ts';
import { browserTransport } from '../src/data/transport.ts';
import type { Ask, BlocksPage, DaysPage, Dispatch, EventBody, Session, Task, Workstream } from '../src/data/types.ts';
import { DEVICE_TOKEN, fakeSockets, startServer, type RunningServer } from './helpers.ts';

describe('recapCacheLookup', () => {
  it('reads a session, task and workstream from their detail or list caches', () => {
    const queryClient = new QueryClient();
    const session: Session = {
      id: 'S1',
      engine: 'claude',
      native_id: 'n',
      machine: 'M1',
      cwd: '/w',
      state: 'working',
      started: 0,
      last_activity: 0,
      task: 'T1',
      workstream: 'W1',
    };
    queryClient.setQueryData(keys.sessions.detail('S1'), session);
    const task: Task = {
      id: 'T1',
      key: 'T-1',
      project: 'P1',
      title: 't',
      description: '',
      status: 'todo',
      priority: 'none',
      labels: [],
      blocked_by: [],
      accept_auto: false,
      subtasks: [],
      workstream: 'W1',
    };
    // Only in a list cache, never fetched by itself: the lookup falls back to scanning lists.
    queryClient.setQueryData(keys.tasks.list({}), [task]);
    const workstream: Workstream = {
      id: 'W1',
      project: 'P1',
      name: 'w',
      status: 'active',
      health: 'on_track',
      locations: [],
      external: [],
    };
    queryClient.setQueryData(keys.workstreams.detail('W1'), workstream);

    const cache = recapCacheLookup(queryClient);
    expect(cache.session('S1')).toEqual({ task: 'T1', workstream: 'W1' });
    expect(cache.task('T1')).toEqual({ workstream: 'W1', project: 'P1' });
    expect(cache.workstream('W1')).toEqual({ project: 'P1' });
    expect(cache.session('S9')).toBeUndefined();
    expect(cache.task('T9')).toBeUndefined();
    expect(cache.workstream('W9')).toBeUndefined();
  });

  it('reads a dispatch or an ask from any cached list', () => {
    const queryClient = new QueryClient();
    const dispatch: Dispatch = { id: 'D1', task: 'T1', agent: 'M1', session: 'S1', brief: '', started: 0 };
    queryClient.setQueryData(keys.dispatches, [dispatch]);
    const ask: Ask = {
      id: 'A1',
      kind: 'question',
      from: 'M1',
      to: 'M2',
      title: 't',
      body: '',
      options: [],
      receipts: [],
      state: 'open',
      created: 0,
      task: 'T1',
    };
    queryClient.setQueryData(keys.asks.list({}), [ask]);

    const cache = recapCacheLookup(queryClient);
    expect(cache.dispatch('D1')).toEqual({ task: 'T1', session: 'S1' });
    expect(cache.ask('A1')).toEqual({ task: 'T1' });
    expect(cache.dispatch('D9')).toBeUndefined();
    expect(cache.ask('A9')).toBeUndefined();
  });
});

describe('recapKeysForScope', () => {
  it('invalidates nothing for an undefined scope, and every recap key (as a prefix) for everything', () => {
    const queryClient = new QueryClient();
    expect(recapKeysForScope(undefined, queryClient)).toEqual({ prefix: [], exact: [] });
    expect(recapKeysForScope('everything', queryClient)).toEqual({ prefix: [keys.recaps.all], exact: [] });
  });

  it('includes a mounted unfiltered blocks query by its exact key, and matching filtered queries as prefixes', () => {
    const queryClient = new QueryClient();
    const page: BlocksPage = { blocks: [], at_start: true };
    const days: DaysPage = { days: [], at_start: true };
    queryClient.setQueryData(keys.recaps.blocks({}), page);
    queryClient.setQueryData(keys.recaps.blocks({ task: 'T1' }), page);
    queryClient.setQueryData(keys.recaps.blocks({ task: 'T9' }), page); // not in scope
    queryClient.setQueryData(keys.recaps.days({ workstream: 'W1' }, 0), days);
    queryClient.setQueryData(keys.recaps.days({ project: 'P9' }, 0), days); // not in scope

    const touched = recapKeysForScope({ session: [], task: ['T1'], workstream: ['W1'], project: [] }, queryClient);
    expect(touched.exact).toEqual([keys.recaps.blocks({})]);
    expect(touched.prefix).toEqual(expect.arrayContaining([keys.recaps.blocks({ task: 'T1' }), keys.recaps.days({ workstream: 'W1' }, 0)]));
    expect(touched.prefix).not.toContainEqual(keys.recaps.blocks({ task: 'T9' }));
    expect(touched.prefix).not.toContainEqual(keys.recaps.days({ project: 'P9' }, 0));
  });

  it('finds an unfiltered blocks query mounted with a page-size override too, by its own exact key', () => {
    const queryClient = new QueryClient();
    const page: BlocksPage = { blocks: [], at_start: true };
    queryClient.setQueryData([...keys.recaps.blocks({}), { limit: 5 }], page);
    const touched = recapKeysForScope({ session: [], task: ['T1'], workstream: [], project: [] }, queryClient);
    expect(touched.exact).toEqual([[...keys.recaps.blocks({}), { limit: 5 }]]);
  });

  it('matches a filtered query however it is paged: its key is a prefix of a page-sized variant too', () => {
    const queryClient = new QueryClient();
    const page: BlocksPage = { blocks: [], at_start: true };
    queryClient.setQueryData([...keys.recaps.blocks({ task: 'T1' }), { limit: 5 }], page);
    const touched = recapKeysForScope({ session: [], task: ['T1'], workstream: [], project: [] }, queryClient);
    expect(touched.prefix).toContainEqual([...keys.recaps.blocks({ task: 'T1' }), { limit: 5 }]);
  });
});

describe('recap invalidation against the mock hub', () => {
  let hub: RunningServer;
  let api: Api;
  let queryClient: QueryClient;
  let live: Live | undefined;

  const TASK = '01JB000000000000000TSK0004'; // in workstream 01JB000000000000000WST0002, project 01JB000000000000000PRJ0001
  const WORKSTREAM = '01JB000000000000000WST0002';
  // A well-formed id that names no real task, so no simulated background activity can ever touch
  // it: a filtered query under it stays untouched for as long as the test cares to check.
  const UNRELATED_TASK = '01JB000000000000000TSKZZZZ';

  afterEach(async () => {
    live?.stop();
    live = undefined;
    queryClient?.clear();
    await hub?.close();
  });

  async function connect(): Promise<void> {
    hub = await startServer({ port: 0 });
    api = createApi({ baseUrl: hub.url, token: DEVICE_TOKEN });
    queryClient = new QueryClient({ defaultOptions: { queries: { staleTime: Infinity, retry: false } } });
    live = createLive({ queryClient, transport: browserTransport({ baseUrl: hub.url, token: DEVICE_TOKEN }), windowMs: 20 });
    live.start();
    await vi.waitFor(() => expect(live?.store.getState().synced).toBe(true));
  }

  const invalidatedKeys = () =>
    queryClient
      .getQueryCache()
      .getAll()
      .filter((q) => q.state.isInvalidated)
      .map((q) => q.queryKey);

  it('a task_moved event invalidates the unfiltered, task-filtered and workstream-day recap keys, and leaves an unrelated one alone', async () => {
    await connect();
    await queryClient.fetchQuery({ queryKey: keys.tasks.detail(TASK), queryFn: () => api.task(TASK) });
    await queryClient.fetchQuery({ queryKey: keys.workstreams.detail(WORKSTREAM), queryFn: () => api.workstream(WORKSTREAM) });
    await Promise.all([
      queryClient.fetchQuery({ queryKey: keys.recaps.blocks({}), queryFn: () => api.recapBlocks() }),
      queryClient.fetchQuery({ queryKey: keys.recaps.blocks({ task: TASK }), queryFn: () => api.recapBlocks({ task: TASK }) }),
      queryClient.fetchQuery({
        queryKey: keys.recaps.days({ workstream: WORKSTREAM }, 0),
        queryFn: () => api.recapDays({ workstream: WORKSTREAM }, 0),
      }),
      // Unrelated: a different task's blocks, which this move does not touch.
      queryClient.fetchQuery({
        queryKey: keys.recaps.blocks({ task: UNRELATED_TASK }),
        queryFn: () => api.recapBlocks({ task: UNRELATED_TASK }),
      }),
    ]);

    const rev = live?.stream.rev ?? -1;
    await api.moveTask(TASK, 'review');
    await vi.waitFor(() => expect(live?.stream.rev).toBe(rev + 1));
    await vi.waitFor(() => expect(invalidatedKeys()).toEqual(expect.arrayContaining([keys.recaps.blocks({})])));
    const touched = invalidatedKeys();
    expect(touched).toEqual(
      expect.arrayContaining([keys.recaps.blocks({}), keys.recaps.blocks({ task: TASK }), keys.recaps.days({ workstream: WORKSTREAM }, 0)]),
    );
    expect(touched).not.toContainEqual(keys.recaps.blocks({ task: UNRELATED_TASK }));
  });

  it('does not touch recap keys for an event the contract says is not activity', async () => {
    await connect();
    await queryClient.fetchQuery({ queryKey: keys.recaps.blocks({}), queryFn: () => api.recapBlocks() });
    const rev = live?.stream.rev ?? -1;
    // A project create emits `project_created`, which the contract says changes no recap.
    await api.createProject({ key: 'ZZQ', name: 'Not a recap event' });
    await vi.waitFor(() => expect(live?.stream.rev).toBe(rev + 1));
    await new Promise((r) => setTimeout(r, 50));
    expect(invalidatedKeys()).not.toContainEqual(keys.recaps.blocks({}));
  });
});

describe('recap invalidation with a scripted stream', () => {
  const queryClients: QueryClient[] = [];
  const lives: Live[] = [];

  afterEach(() => {
    for (const live of lives.splice(0)) live.stop();
    for (const qc of queryClients.splice(0)) qc.clear();
  });

  function setup() {
    const queryClient = new QueryClient({ defaultOptions: { queries: { staleTime: Infinity, retry: false } } });
    const { factory, sockets } = fakeSockets();
    const live = createLive({
      queryClient,
      transport: browserTransport({ baseUrl: 'http://127.0.0.1:47317', socket: factory }),
      windowMs: 10,
    });
    queryClients.push(queryClient);
    lives.push(live);
    live.start();
    sockets[0]?.send({ type: 'hello', rev: 1, log: 'LOG-A' });
    let rev = 1;
    const emit = (body: EventBody) => {
      rev += 1;
      sockets.at(-1)?.send({ type: 'events', from_rev: rev, to_rev: rev, events: [{ id: `E${rev}`, at: 0, workspace: 'W', author: 'M', body }] });
    };
    return { queryClient, live, emit };
  }

  const invalidatedKeys = (queryClient: QueryClient) =>
    queryClient
      .getQueryCache()
      .getAll()
      .filter((q) => q.state.isInvalidated)
      .map((q) => q.queryKey);

  it('member_added invalidates every recap key, filtered or not', async () => {
    const { queryClient, emit } = setup();
    const page: BlocksPage = { blocks: [], at_start: true };
    queryClient.setQueryData(keys.recaps.blocks({ task: 'T1' }), page);
    const member = { id: 'M2', kind: 'agent', handle: '@new', name: 'New' } as const;
    emit({ type: 'member_added', data: { member } });
    await vi.waitFor(() => expect(invalidatedKeys(queryClient)).toContainEqual(keys.recaps.blocks({ task: 'T1' })));
  });

  it('invalidates every recap key when the cache cannot resolve a link', async () => {
    const { queryClient, emit } = setup();
    const page: BlocksPage = { blocks: [], at_start: true };
    queryClient.setQueryData(keys.recaps.blocks({ task: 'T9' }), page); // unrelated to the unresolvable task
    // No task 'T1' is cached, so its workstream and project cannot be resolved.
    emit({ type: 'task_moved', data: { task: 'T1', from: 'todo', to: 'review', mover: { kind: 'person' } } });
    await vi.waitFor(() => expect(invalidatedKeys(queryClient)).toContainEqual(keys.recaps.blocks({ task: 'T9' })));
  });

  it('leaves recap keys alone for an event that is not activity', async () => {
    const { queryClient, emit } = setup();
    const page: BlocksPage = { blocks: [], at_start: true };
    queryClient.setQueryData(keys.recaps.blocks({}), page);
    const before = queryClient.getQueryState(keys.recaps.blocks({}))?.dataUpdateCount;
    emit({ type: 'machine_liveness', data: { machine: 'M1', liveness: 'live' } });
    await new Promise((r) => setTimeout(r, 50));
    expect(queryClient.getQueryState(keys.recaps.blocks({}))?.isInvalidated).toBe(false);
    expect(queryClient.getQueryState(keys.recaps.blocks({}))?.dataUpdateCount).toBe(before);
  });

  it('refetches active recap queries, so an observer sees fresh data without a reload', async () => {
    const { queryClient, emit } = setup();
    let calls = 0;
    const queryFn = vi.fn(async () => {
      calls += 1;
      return { blocks: [], at_start: true } as BlocksPage;
    });
    const observer = new QueryObserver(queryClient, { queryKey: keys.recaps.blocks({}), queryFn });
    const unsubscribe = observer.subscribe(() => {});
    await vi.waitFor(() => expect(calls).toBe(1));
    emit({ type: 'task_created', data: { task: { id: 'T1', key: 'T-1', project: 'P1', title: 't', description: '', status: 'todo', priority: 'none', labels: [], blocked_by: [], accept_auto: false, subtasks: [] } } });
    await vi.waitFor(() => expect(calls).toBe(2));
    unsubscribe();
  });
});
