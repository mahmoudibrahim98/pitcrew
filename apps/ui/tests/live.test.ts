import { QueryClient, QueryObserver } from '@tanstack/react-query';
import { afterEach, beforeEach, describe, expect, it, vi } from 'vitest';
import { createApi, type Api } from '../src/data/api.ts';
import { keys } from '../src/data/keys.ts';
import { connectLive } from '../src/data/live.ts';
import type { StreamClient } from '../src/data/stream.ts';
import { DEVICE_TOKEN, startServer, type RunningServer } from './helpers.ts';

describe('live cache', () => {
  let hub: RunningServer;
  let api: Api;
  let queryClient: QueryClient;
  let live: StreamClient | undefined;

  beforeEach(async () => {
    hub = await startServer({ port: 0 });
    api = createApi({ baseUrl: hub.url, token: DEVICE_TOKEN });
    queryClient = new QueryClient({ defaultOptions: { queries: { staleTime: Infinity, retry: false } } });
  });

  afterEach(async () => {
    live?.stop();
    queryClient.clear();
    await hub.close();
  });

  it('a task_moved event invalidates exactly that task, the task lists and its workstream', async () => {
    const tasks = await queryClient.fetchQuery({ queryKey: keys.tasks.list(), queryFn: () => api.tasks() });
    const task = tasks.find((t) => t.status === 'todo' && t.workstream !== undefined);
    const other = tasks.find((t) => t.id !== task?.id && t.workstream !== task?.workstream);
    if (task?.workstream === undefined || other?.workstream === undefined) {
      throw new Error('the fixture needs todo tasks in two workstreams');
    }
    const workspace = await queryClient.fetchQuery({ queryKey: keys.workspace, queryFn: () => api.workspace() });
    await Promise.all([
      queryClient.fetchQuery({ queryKey: keys.tasks.detail(task.id), queryFn: () => api.task(task.id) }),
      queryClient.fetchQuery({ queryKey: keys.tasks.detail(other.id), queryFn: () => api.task(other.id) }),
      queryClient.fetchQuery({ queryKey: keys.tasks.list({ status: ['todo'] }), queryFn: () => api.tasks({ status: ['todo'] }) }),
      queryClient.fetchQuery({ queryKey: keys.workstreams.detail(task.workstream), queryFn: () => api.workstream(task.workstream as string) }),
      queryClient.fetchQuery({ queryKey: keys.workstreams.detail(other.workstream), queryFn: () => api.workstream(other.workstream as string) }),
      queryClient.fetchQuery({ queryKey: keys.projects.list(), queryFn: () => api.projects() }),
      queryClient.fetchQuery({ queryKey: keys.sessions.list(), queryFn: () => api.sessions() }),
    ]);

    live = connectLive({ queryClient, baseUrl: hub.url, token: DEVICE_TOKEN, since: workspace.rev });
    await vi.waitFor(() => expect(live?.status).toBe('live'));

    await api.moveTask(task.id, 'review');
    await vi.waitFor(() => expect(live?.rev).toBe(workspace.rev + 1));

    const invalidated = queryClient
      .getQueryCache()
      .getAll()
      .filter((q) => q.state.isInvalidated)
      .map((q) => q.queryKey);
    expect(invalidated).toEqual(
      expect.arrayContaining([
        keys.tasks.list(),
        keys.tasks.list({ status: ['todo'] }),
        keys.tasks.detail(task.id),
        keys.workstreams.detail(task.workstream),
      ]),
    );
    expect(invalidated).toHaveLength(4);
  });

  it('refetches active queries so observers see the new state without a reload', async () => {
    const workspace = await api.workspace();
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

    live = connectLive({ queryClient, baseUrl: hub.url, token: DEVICE_TOKEN, since: workspace.rev });
    await vi.waitFor(() => expect(live?.status).toBe('live'));
    await api.moveTask(task.id, 'in_progress');
    await vi.waitFor(() => expect(seen.at(-1)).toBe('in_progress'));
    unsubscribe();
  });
});
