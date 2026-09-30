import { QueryClient, QueryObserver } from '@tanstack/react-query';
import { afterEach, beforeEach, describe, expect, it, vi } from 'vitest';
import { ApiError, createApi, type Api } from '../src/data/api.ts';
import { keys } from '../src/data/keys.ts';
import { createLive, Invalidator, type Live } from '../src/data/live.ts';
import type { EventBody, Session, Task } from '../src/data/types.ts';
import { DEVICE_TOKEN, fakeSockets, startServer, type RunningServer } from './helpers.ts';

describe('live cache against the mock hub', () => {
  let hub: RunningServer;
  let api: Api;
  let queryClient: QueryClient;
  let live: Live | undefined;

  beforeEach(async () => {
    hub = await startServer({ port: 0 });
    api = createApi({ baseUrl: hub.url, token: DEVICE_TOKEN });
    queryClient = new QueryClient({ defaultOptions: { queries: { staleTime: Infinity, retry: false } } });
  });

  afterEach(async () => {
    live?.stop();
    live = undefined;
    queryClient.clear();
    await hub.close();
  });

  async function connect(windowMs = 20): Promise<number> {
    live = createLive({ queryClient, baseUrl: hub.url, token: DEVICE_TOKEN, windowMs });
    live.start();
    await vi.waitFor(() => expect(live?.store.getState().synced).toBe(true));
    return live.stream.rev ?? -1;
  }

  const invalidatedKeys = () =>
    queryClient
      .getQueryCache()
      .getAll()
      .filter((q) => q.state.isInvalidated)
      .map((q) => q.queryKey);

  it('a task_moved event invalidates exactly that task, the task lists and its workstream', async () => {
    const rev = await connect();
    const tasks = await queryClient.fetchQuery({ queryKey: keys.tasks.list(), queryFn: () => api.tasks() });
    const task = tasks.find((t) => t.status === 'todo' && t.workstream !== undefined);
    const other = tasks.find((t) => t.id !== task?.id && t.workstream !== task?.workstream);
    if (task?.workstream === undefined || other?.workstream === undefined) {
      throw new Error('the fixture needs todo tasks in two workstreams');
    }
    const ws = task.workstream;
    const otherWs = other.workstream;
    await Promise.all([
      queryClient.fetchQuery({ queryKey: keys.tasks.detail(task.id), queryFn: () => api.task(task.id) }),
      queryClient.fetchQuery({ queryKey: keys.tasks.detail(other.id), queryFn: () => api.task(other.id) }),
      queryClient.fetchQuery({ queryKey: keys.tasks.list({ status: ['todo'] }), queryFn: () => api.tasks({ status: ['todo'] }) }),
      queryClient.fetchQuery({ queryKey: keys.workstreams.detail(ws), queryFn: () => api.workstream(ws) }),
      queryClient.fetchQuery({ queryKey: keys.workstreams.detail(otherWs), queryFn: () => api.workstream(otherWs) }),
      queryClient.fetchQuery({ queryKey: keys.projects.list(), queryFn: () => api.projects() }),
      queryClient.fetchQuery({ queryKey: keys.sessions.list(), queryFn: () => api.sessions() }),
    ]);

    await api.moveTask(task.id, 'review');
    await vi.waitFor(() => expect(live?.stream.rev).toBe(rev + 1));
    await vi.waitFor(() => expect(invalidatedKeys()).toHaveLength(4));
    expect(invalidatedKeys()).toEqual(
      expect.arrayContaining([
        keys.tasks.list(),
        keys.tasks.list({ status: ['todo'] }),
        keys.tasks.detail(task.id),
        keys.workstreams.detail(ws),
      ]),
    );
  });

  it('refetches active queries so observers see the new state without a reload', async () => {
    await connect();
    const [task] = await api.tasks({ status: ['todo'] });
    if (task === undefined) throw new Error('the fixture has no todo task');
    const observer = new QueryObserver(queryClient, {
      queryKey: keys.tasks.detail(task.id),
      queryFn: () => api.task(task.id),
    });
    const seen: string[] = [];
    const unsubscribe = observer.subscribe((result) => {
      if (result.data !== undefined) seen.push(result.data.status);
    });
    await vi.waitFor(() => expect(seen).toContain('todo'));
    await api.moveTask(task.id, 'in_progress');
    await vi.waitFor(() => expect(seen.at(-1)).toBe('in_progress'));
    unsubscribe();
  });

  it('coalesces a burst of events into one refetch per query', async () => {
    await connect(250);
    const todo = await api.tasks({ status: ['todo'] });
    if (todo.length < 2) throw new Error('the fixture needs two todo tasks');
    const queryFn = vi.fn(() => api.tasks());
    const observer = new QueryObserver(queryClient, { queryKey: keys.tasks.list(), queryFn });
    const unsubscribe = observer.subscribe(() => {});
    await vi.waitFor(() => expect(queryFn).toHaveBeenCalledTimes(1));
    await vi.waitFor(() => expect(observer.getCurrentResult().isFetching).toBe(false));

    await Promise.all(todo.slice(0, 2).map((t) => api.moveTask(t.id, 'review')));
    await vi.waitFor(() =>
      expect(observer.getCurrentResult().data?.filter((t) => t.status === 'review').length).toBeGreaterThanOrEqual(
        todo.slice(0, 2).length,
      ),
    );
    expect(queryFn).toHaveBeenCalledTimes(2);
    unsubscribe();
  });

  it('writes task_created and subtasks_replaced into the cache without refetching', async () => {
    await connect();
    const projects = await api.projects();
    const project = projects[0];
    if (project === undefined) throw new Error('the fixture has no projects');
    const all = await queryClient.fetchQuery({ queryKey: keys.tasks.list(), queryFn: () => api.tasks() });
    await queryClient.fetchQuery({
      queryKey: keys.tasks.list({ status: ['done'] }),
      queryFn: () => api.tasks({ status: ['done'] }),
    });

    const created = await api.request<Task>('POST', '/v1/tasks', {
      body: { project: project.id, title: 'A synthetic task' },
    });
    await vi.waitFor(() =>
      expect(queryClient.getQueryData<Task[]>(keys.tasks.list())).toHaveLength(all.length + 1),
    );
    // A todo task does not belong in the done list.
    expect(queryClient.getQueryData<Task[]>(keys.tasks.list({ status: ['done'] }))?.some((t) => t.id === created.id)).toBe(false);
    expect(queryClient.getQueryData<Task>(keys.tasks.detail(created.id))?.title).toBe('A synthetic task');

    const subtasks = [
      { id: '01JB0000000000000000000S01', text: 'First step', done: false, source: { kind: 'human' } },
    ];
    await api.request<Task>('PUT', `/v1/tasks/${created.id}/subtasks`, { body: subtasks });
    await vi.waitFor(() =>
      expect(queryClient.getQueryData<Task>(keys.tasks.detail(created.id))?.subtasks.map((s) => s.text)).toEqual([
        'First step',
      ]),
    );
    const inList = queryClient.getQueryData<Task[]>(keys.tasks.list())?.find((t) => t.id === created.id);
    expect(inList?.subtasks).toHaveLength(1);
    expect(invalidatedKeys()).toEqual([]);
  });
});

describe('invalidator', () => {
  afterEach(() => {
    vi.useRealTimers();
  });

  it('does not cancel a fetch in flight, and invalidates again once it settles', async () => {
    const queryClient = new QueryClient({ defaultOptions: { queries: { staleTime: Infinity, retry: false } } });
    const resolvers: ((value: number) => void)[] = [];
    const queryFn = vi.fn(() => new Promise<number>((resolve) => resolvers.push(resolve)));
    const observer = new QueryObserver(queryClient, { queryKey: ['thing'], queryFn });
    const unsubscribe = observer.subscribe(() => {});
    await vi.waitFor(() => expect(queryFn).toHaveBeenCalledTimes(1));

    const invalidator = new Invalidator(queryClient, { windowMs: 10 });
    invalidator.add([['thing']]);
    await new Promise((r) => setTimeout(r, 50));
    expect(queryFn).toHaveBeenCalledTimes(1); // not cancelled and restarted

    resolvers[0]?.(1); // this answer may predate the event
    await vi.waitFor(() => expect(queryFn).toHaveBeenCalledTimes(2));
    resolvers[1]?.(2);
    await vi.waitFor(() => expect(observer.getCurrentResult().data).toBe(2));
    invalidator.stop();
    unsubscribe();
  });

  it('rate-limits invalidating everything', () => {
    vi.useFakeTimers();
    const queryClient = new QueryClient();
    const spy = vi.spyOn(queryClient, 'invalidateQueries');
    const invalidator = new Invalidator(queryClient, { windowMs: 250, everythingMs: 5_000 });
    invalidator.add([[]]);
    vi.advanceTimersByTime(250);
    expect(spy).toHaveBeenCalledTimes(1);
    invalidator.add([[]]);
    vi.advanceTimersByTime(1_000);
    expect(spy).toHaveBeenCalledTimes(1);
    vi.advanceTimersByTime(4_000);
    expect(spy).toHaveBeenCalledTimes(2);
    invalidator.stop();
  });

  it('does not hold other keys while everything waits for its rate limit', () => {
    vi.useFakeTimers();
    const queryClient = new QueryClient();
    const spy = vi.spyOn(queryClient, 'invalidateQueries');
    const invalidator = new Invalidator(queryClient, { windowMs: 250, everythingMs: 5_000 });
    invalidator.add([[]]);
    vi.advanceTimersByTime(250);
    invalidator.add([[], ['tasks', 'list']]);
    vi.advanceTimersByTime(250);
    expect(spy).toHaveBeenCalledTimes(2);
    expect(spy.mock.calls[1]?.[0]).toMatchObject({ queryKey: ['tasks', 'list'] });
    invalidator.stop();
  });
});

describe('live cache with a scripted stream', () => {
  const queryClients: QueryClient[] = [];
  const lives: Live[] = [];

  afterEach(() => {
    for (const live of lives.splice(0)) live.stop();
    for (const qc of queryClients.splice(0)) qc.clear();
  });

  function setup(options: { probe?: () => Promise<unknown> } = {}) {
    const queryClient = new QueryClient({ defaultOptions: { queries: { staleTime: Infinity, retry: false } } });
    const { factory, sockets } = fakeSockets();
    const live = createLive({
      queryClient,
      baseUrl: 'http://127.0.0.1:47317',
      socket: factory,
      windowMs: 10,
      backoff: { initialMs: 5, maxMs: 10 },
      probeAfter: 2,
      ...(options.probe === undefined ? {} : { probe: options.probe }),
    });
    queryClients.push(queryClient);
    lives.push(live);
    live.start();
    sockets[0]?.send({ type: 'hello', rev: 1, log: 'LOG-A' });
    let rev = 1;
    const emit = (body: EventBody) => {
      rev += 1;
      sockets.at(-1)?.send({
        type: 'events',
        from_rev: rev,
        to_rev: rev,
        events: [{ id: `E${rev}`, at: 0, workspace: 'W', author: 'M', body }],
      });
    };
    return { queryClient, live, sockets, emit };
  }

  const task = (id: string, status: Task['status'] = 'todo'): Task => ({
    id,
    key: `T-${id}`,
    project: 'P',
    title: id,
    description: '',
    status,
    priority: 'none',
    labels: [],
    blocked_by: [],
    accept_auto: false,
    subtasks: [],
  });

  it('refetches after a fetch that was in flight when a patch landed', async () => {
    const { queryClient, emit } = setup();
    const resolvers: ((tasks: Task[]) => void)[] = [];
    const queryFn = vi.fn(() => new Promise<Task[]>((resolve) => resolvers.push(resolve)));
    const observer = new QueryObserver(queryClient, { queryKey: keys.tasks.list(), queryFn });
    const unsubscribe = observer.subscribe(() => {});
    await vi.waitFor(() => expect(queryFn).toHaveBeenCalledTimes(1));

    emit({ type: 'task_created', data: { task: task('NEW') } });
    resolvers[0]?.([task('OLD')]); // answered before the task existed
    await vi.waitFor(() => expect(queryFn).toHaveBeenCalledTimes(2));
    resolvers[1]?.([task('OLD'), task('NEW')]);
    await vi.waitFor(() => expect(observer.getCurrentResult().data?.map((t) => t.id)).toEqual(['OLD', 'NEW']));
    unsubscribe();
  });

  it('falls back to refetching when a patch cannot apply', async () => {
    const { queryClient, emit } = setup();
    queryClient.setQueryData(keys.tasks.list(), { not: 'a list' });
    const warn = vi.spyOn(console, 'warn').mockImplementation(() => {});
    emit({ type: 'task_created', data: { task: task('NEW') } });
    await vi.waitFor(() =>
      expect(queryClient.getQueryCache().find({ queryKey: keys.tasks.list() })?.state.isInvalidated).toBe(true),
    );
    expect(warn).toHaveBeenCalled();
    warn.mockRestore();
  });

  it('takes a rediscovered session out of a list it no longer matches', () => {
    const { queryClient, emit } = setup();
    const session: Session = {
      id: 'S1',
      engine: 'claude',
      native_id: 'n',
      machine: 'M1',
      cwd: '/work',
      state: 'working',
      started: 0,
      last_activity: 0,
    };
    queryClient.setQueryData(keys.sessions.list({ state: 'working' }), [session]);
    queryClient.setQueryData(keys.sessions.list(), [session]);
    emit({ type: 'session_discovered', data: { session: { ...session, state: 'idle' } } });
    expect(queryClient.getQueryData<Session[]>(keys.sessions.list({ state: 'working' }))).toEqual([]);
    expect(queryClient.getQueryData<Session[]>(keys.sessions.list())?.[0]?.state).toBe('idle');
  });

  it('refetches failed active queries when the stream comes back', async () => {
    const { queryClient, live, sockets } = setup();
    let fail = true;
    const queryFn = vi.fn(async () => {
      if (fail) throw new Error('hub unreachable');
      return 'ok';
    });
    const observer = new QueryObserver(queryClient, { queryKey: ['thing'], queryFn });
    const unsubscribe = observer.subscribe(() => {});
    await vi.waitFor(() => expect(observer.getCurrentResult().status).toBe('error'));

    fail = false;
    sockets[0]?.drop();
    await vi.waitFor(() => expect(sockets).toHaveLength(2));
    sockets[1]?.send({ type: 'hello', rev: 1, log: 'LOG-A' });
    expect(live.store.getState().status).toBe('live');
    await vi.waitFor(() => expect(observer.getCurrentResult().data).toBe('ok'));
    expect(queryFn).toHaveBeenCalledTimes(2);
    unsubscribe();
  });

  it('says once that the token was rejected, after repeated failures', async () => {
    const warn = vi.spyOn(console, 'warn').mockImplementation(() => {});
    const probe = vi.fn(async () => {
      throw new ApiError('unauthorized', 'Unknown token', 401);
    });
    const { live, sockets } = setup({ probe });
    for (let i = 0; i < 4; i++) {
      const count = sockets.length;
      sockets.at(-1)?.drop();
      await vi.waitFor(() => expect(sockets.length).toBe(count + 1));
    }
    await vi.waitFor(() => expect(live.store.getState().problem).toBe('unauthorized'));
    expect(probe).toHaveBeenCalledTimes(1);
    expect(warn).toHaveBeenCalledTimes(1);

    sockets.at(-1)?.send({ type: 'hello', rev: 1, log: 'LOG-A' });
    expect(live.store.getState().problem).toBeUndefined();
    warn.mockRestore();
  });
});
