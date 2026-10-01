import { afterAll, beforeAll, describe, expect, it } from 'vitest';
import { ApiError, createApi } from '../src/data/api.ts';
import type { Subtask } from '../src/data/types.ts';
import { AGENT_TOKEN, DEVICE_TOKEN, startServer, type RunningServer } from './helpers.ts';

async function failure(promise: Promise<unknown>): Promise<ApiError> {
  try {
    await promise;
  } catch (error) {
    if (error instanceof ApiError) return error;
    throw error;
  }
  throw new Error('expected the request to fail');
}

describe('api client against the mock hub', () => {
  let hub: RunningServer;

  beforeAll(async () => {
    hub = await startServer({ port: 0 });
  });

  afterAll(() => hub.close());

  it('sends the bearer token and reads JSON', async () => {
    const api = createApi({ baseUrl: hub.url, token: DEVICE_TOKEN });
    const me = await api.me();
    expect(me.handle).toBe('@sam');
    const { workspace, rev } = await api.workspace();
    expect(workspace.name).not.toBe('');
    expect(rev).toBeGreaterThan(0);
  });

  it('repeats a filter given as a list', async () => {
    const api = createApi({ baseUrl: hub.url, token: DEVICE_TOKEN });
    const tasks = await api.tasks({ status: ['todo', 'review'] });
    expect(tasks.length).toBeGreaterThan(0);
    expect(new Set(tasks.map((t) => t.status))).toEqual(new Set(['todo', 'review']));
  });

  it('maps 401 unauthorized', async () => {
    const error = await failure(createApi({ baseUrl: hub.url }).projects());
    expect(error).toMatchObject({ code: 'unauthorized', status: 401 });
    expect(error.message).not.toBe('');
  });

  it('maps 404 not_found', async () => {
    const api = createApi({ baseUrl: hub.url, token: DEVICE_TOKEN });
    expect(await failure(api.task('NOPE-999'))).toMatchObject({ code: 'not_found', status: 404 });
  });

  it('maps 409 conflict for a move the rules refuse', async () => {
    const api = createApi({ baseUrl: hub.url, token: DEVICE_TOKEN });
    const [task] = await api.tasks({ status: ['todo'] });
    if (task === undefined) throw new Error('the fixture has no todo task');
    expect(await failure(api.moveTask(task.id, 'todo'))).toMatchObject({ code: 'conflict', status: 409 });
  });

  it('maps 403 forbidden for an agent moving a task not its own', async () => {
    const device = createApi({ baseUrl: hub.url, token: DEVICE_TOKEN });
    const agent = createApi({ baseUrl: hub.url, token: AGENT_TOKEN });
    const me = await agent.me();
    const tasks = await device.tasks({ status: ['todo'] });
    const other = tasks.find((t) => t.assignee !== me.id);
    if (other === undefined) throw new Error('the fixture has no todo task for someone else');
    expect(await failure(agent.moveTask(other.id, 'in_progress'))).toMatchObject({
      code: 'forbidden',
      status: 403,
    });
  });

  it('maps 400 invalid for an unknown enum value', async () => {
    const api = createApi({ baseUrl: hub.url, token: DEVICE_TOKEN });
    const [task] = await api.tasks();
    if (task === undefined) throw new Error('the fixture has no tasks');
    const error = await failure(api.moveTask(task.id, 'sideways' as never));
    expect(error).toMatchObject({ code: 'invalid', status: 400 });
  });
});

describe('api client writes, briefs and activity against the mock hub', () => {
  let hub: RunningServer;
  const SAM = '01JB000000000000000MEM0001';
  const RUNNER = '01JB000000000000000MEM0003';
  const PAPER = { kind: 'project', id: '01JB000000000000000PRJ0001' } as const;
  const SEED_RUNS = { kind: 'workstream', id: '01JB000000000000000WST0002' } as const;

  beforeAll(async () => {
    hub = await startServer({ port: 0 });
  });

  afterAll(() => hub.close());

  const api = () => createApi({ baseUrl: hub.url, token: DEVICE_TOKEN });

  it('pages activity oldest first, back to the start', async () => {
    const newest = await api().events({ limit: 5 });
    expect(newest.events).toHaveLength(5);
    expect(newest.to_rev - newest.from_rev).toBe(4);
    expect(newest.at_start).toBe(false);
    const older = await api().events({ before: newest.from_rev, limit: 500 });
    expect(older.to_rev).toBe(newest.from_rev - 1);
    expect(older.from_rev).toBe(1);
    expect(older.at_start).toBe(true);
    const task = await api().events({ task: '01JB000000000000000TSK0004' });
    expect(task.events.length).toBeGreaterThan(0);
  });

  it('reads briefs, and edits, pins and accepts them as a person', async () => {
    const all = await api().briefs();
    expect(all.length).toBeGreaterThan(0);
    const paper = await api().brief(PAPER);
    expect(paper?.source).toBe('back_office');
    expect(await api().brief({ kind: 'workstream', id: '01JB000000000000000WST0004' })).toBeUndefined();

    const edited = await api().editBrief(PAPER, { text: 'Seeds 1–4 are in.', pinned: false });
    expect(edited).toMatchObject({ text: 'Seeds 1–4 are in.', source: 'person', pinned: false });
    const pinned = await api().pinBrief(edited, true);
    expect(pinned).toMatchObject({ text: 'Seeds 1–4 are in.', pinned: true });
    const accepted = await api().acceptBrief(SEED_RUNS, { text: 'Seed 3 reran.', next: 'Aggregate.' }, true);
    expect(accepted).toMatchObject({ text: 'Seed 3 reran.', next: 'Aggregate.', pinned: true, source: 'person' });
  });

  it('creates, assigns, comments on, plans and dispatches a task', async () => {
    const task = await api().createTask({ project: PAPER.id, title: 'Check the seed 3 logs' });
    expect(task).toMatchObject({ status: 'todo', title: 'Check the seed 3 logs' });
    expect(task.key).toMatch(/^PAP-\d+$/);

    expect((await api().assignTask(task.id, SAM)).assignee).toBe(SAM);
    expect((await api().assignTask(task.key, null)).assignee).toBeUndefined();

    const comment = await api().comment(task.id, { text: 'Look at epoch 9.', mentions: [SAM] });
    expect(comment.body).toMatchObject({ type: 'comment_posted', data: { text: 'Look at epoch 9.' } });

    const subtasks: Subtask[] = [
      { id: '01JB00000000000000000SBT01', text: 'Open the log', done: false, source: { kind: 'human' } },
    ];
    expect((await api().replaceSubtasks(task.id, subtasks)).subtasks).toEqual(subtasks);

    const dispatch = await api().dispatchTask(task.id, { agent: RUNNER, brief: 'Read the logs.' });
    expect(dispatch).toMatchObject({ task: task.id, agent: RUNNER, brief: 'Read the logs.' });
    expect((await api().task(task.id)).assignee).toBe(RUNNER);
  });

  it('pages a transcript tail-first and drives a session', async () => {
    const newest = await api().transcript('01JB000000000000000SES0001', { limit: 5 });
    expect(newest.items.length).toBeGreaterThan(0);
    expect(newest.at_start).toBe(false);
    const older = await api().transcript('01JB000000000000000SES0001', { before: newest.from, limit: 5 });
    expect(older.to).toBeLessThanOrEqual(newest.from);
    expect(older.items.every((item) => item.offset < newest.from)).toBe(true);

    const live = (await api().sessions()).find(
      (s) => s.machine === '01JB000000000000000MCH0001' && s.state !== 'ended',
    );
    if (live === undefined) throw new Error('the fixture has no live session on this laptop');
    await expect(api().send(live.id, 'Carry on.')).resolves.toBeUndefined();
    await expect(api().keys(live.id, ['escape', 'ctrl_c'])).resolves.toBeUndefined();
    await expect(api().interrupt(live.id)).resolves.toBeUndefined();
    await expect(api().end(live.id, 'graceful')).resolves.toBeUndefined();
    expect(await failure(api().keys(live.id, ['sideways' as never]))).toMatchObject({ code: 'invalid' });
  });

  it('lists machines', async () => {
    expect((await api().machines()).map((m) => m.liveness)).toContain('live');
  });

  it('answers an ask', async () => {
    const [ask] = await api().asks({ to: SAM, state: 'open' });
    if (ask === undefined) throw new Error('the fixture has no open ask to @sam');
    const answered = await api().answerAsk(ask.id, ask.options.length > 0 ? { option: 0 } : { text: 'Yes.' });
    expect(answered.state).toBe('answered');
    expect(await failure(api().answerAsk(ask.id, { text: 'Again.' }))).toMatchObject({ code: 'conflict' });
  });
});

describe('api client error mapping', () => {
  const respond = (response: Response) => createApi({ baseUrl: 'http://hub.localhost', fetch: async () => response });

  it('uses a contract body when there is one', async () => {
    const body = JSON.stringify({ code: 'unavailable', message: 'The GPU box is unreachable.' });
    const error = await failure(respond(new Response(body, { status: 503 })).projects());
    expect(error).toMatchObject({ code: 'unavailable', status: 503, message: 'The GPU box is unreachable.' });
  });

  it('falls back to the status for a body that is not an ApiError', async () => {
    const html = new Response('<h1>Bad gateway</h1>', { status: 502, statusText: 'Bad Gateway' });
    expect(await failure(respond(html).projects())).toMatchObject({ code: 'internal', status: 502 });
    const odd = new Response(JSON.stringify({ code: 'teapot', message: 'no' }), { status: 404 });
    expect(await failure(respond(odd).projects())).toMatchObject({ code: 'not_found', status: 404 });
  });

  it('reports an unreachable hub as unavailable with status 0', async () => {
    const api = createApi({
      baseUrl: 'http://hub.localhost',
      fetch: async () => {
        throw new TypeError('fetch failed');
      },
    });
    expect(await failure(api.projects())).toMatchObject({ code: 'unavailable', status: 0 });
  });

  it('turns a 2xx without JSON into an ApiError', async () => {
    const ok = new Response('<html>captive portal</html>', { status: 200 });
    const error = await failure(respond(ok).projects());
    expect(error).toMatchObject({ code: 'internal', status: 200 });
    expect(error.message).toContain('without JSON');
  });

  it('returns nothing for 204', async () => {
    const api = respond(new Response(null, { status: 204 }));
    await expect(api.request('POST', '/v1/sessions/S/interrupt')).resolves.toBeUndefined();
  });
});
