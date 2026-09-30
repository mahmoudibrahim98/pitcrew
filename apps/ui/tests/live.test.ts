import { QueryClient, QueryObserver } from '@tanstack/react-query';
import { afterEach, beforeEach, describe, expect, it, vi } from 'vitest';
import { createApi, type Api } from '../src/data/api.ts';
import { keys } from '../src/data/keys.ts';
import { createLive, Invalidator, type Live } from '../src/data/live.ts';
import type { Task } from '../src/data/types.ts';
import { DEVICE_TOKEN, startServer, type RunningServer } from './helpers.ts';

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
});
