import { afterAll, beforeAll, describe, expect, it } from 'vitest';
import { ApiError, createApi } from '../src/data/api.ts';
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
