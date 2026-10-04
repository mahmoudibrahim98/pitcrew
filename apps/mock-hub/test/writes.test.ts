// Outward writes (api-v1.md, "Outward writes"): every one approved first. The mock keeps every
// request it "sent" upstream, so these show nothing is sent before an approval, after a denial,
// or twice for one approval or one retry; and that a created issue becomes the task's source.

import assert from 'node:assert/strict';
import { test } from 'node:test';
import { AGENT, DEVICE, ID, call, withServer } from './helpers.ts';
import { sentRequests } from '../src/writes.ts';
import type { Ask, Event, Integration, Task, UpstreamWrite } from '../src/types.ts';

const SEED_RUNS = '01JB000000000000000WST0002';
const IDEA = '01JB000000000000000WST0004';

const github = {
  name: 'Demo repository',
  settings: { kind: 'github', repos: ['example-org/demo-repo'] },
  credential: 'gh_cli',
};

type Server = Parameters<Parameters<typeof withServer>[0]>[0];

async function connect(server: Server, workstream: string, key: string): Promise<Integration> {
  const added = await call<Integration>(server, 'POST', '/v1/integrations', { token: DEVICE, json: github });
  assert.equal(added.status, 201);
  const linked = await call(server, 'PATCH', `/v1/workstreams/${workstream}`, {
    token: DEVICE,
    json: { external: [{ system: 'github', key }] },
  });
  assert.equal(linked.status, 200);
  assert.equal((await call(server, 'POST', `/v1/integrations/${added.body.id}/sync`, { token: DEVICE })).status, 202);
  return added.body;
}

const writes = async (server: Server, query = ''): Promise<UpstreamWrite[]> =>
  (await call<UpstreamWrite[]>(server, 'GET', `/v1/writes${query}`, { token: DEVICE })).body;

const sent = (server: Server) => sentRequests(server.hub).filter((r) => r.method !== 'GET');

const answer = (server: Server, ask: string, option: number) =>
  call(server, 'POST', `/v1/asks/${ask}/answer`, { token: DEVICE, json: { option } });

test('the writes routes are device-only', async () => {
  await withServer(async (server) => {
    for (const [method, path] of [
      ['GET', '/v1/writes'],
      ['POST', '/v1/writes'],
      ['GET', `/v1/writes/${ID.pap1}`],
      ['POST', `/v1/writes/${ID.pap1}/retry`],
    ] as const) {
      const options = method === 'GET' ? { token: AGENT } : { token: AGENT, json: {} };
      assert.equal((await call(server, method, path, options)).status, 403, `${method} ${path}`);
    }
    assert.equal((await call(server, 'GET', '/v1/writes/01J00000000000000000000000', { token: DEVICE })).status, 404);
    assert.equal((await call(server, 'GET', '/v1/writes?state=nope', { token: DEVICE })).status, 400);
  });
});

test('nothing is sent before an approval, after a denial, or twice', async () => {
  await withServer(async (server) => {
    await connect(server, SEED_RUNS, 'example-org/demo-repo#milestone:1');
    const task = server.hub.tasks.find((t) => t.source?.key === 'example-org/demo-repo#1');
    assert.ok(task);
    assert.deepEqual(sent(server), [], 'a sync only reads');

    // A person closes the task: an approval ask, and nothing sent.
    assert.equal((await call(server, 'POST', `/v1/tasks/${task.id}/move`, { token: DEVICE, json: { to: 'done' } })).status, 200);
    let list = await writes(server, `?task=${task.id}`);
    assert.equal(list.length, 1);
    const first = list[0];
    assert.ok(first);
    assert.equal(first.state, 'pending');
    assert.equal(first.proposal.operation, 'close');
    assert.deepEqual(first.proposal.before, { state: 'open' });
    assert.deepEqual(first.proposal.after, { state: 'closed', close_reason: 'completed' });
    const ask = server.hub.asks.find((a: Ask) => a.id === first.proposal.ask);
    assert.equal(ask?.kind, 'approval');
    assert.deepEqual(ask?.options, ['Send', "Don't send"]);
    assert.ok(ask?.body.includes('state: open → closed (completed)'));
    assert.deepEqual(sent(server), []);

    // Denied: recorded as not sent.
    assert.equal((await answer(server, first.proposal.ask, 1)).status, 200);
    const denied = (await call<UpstreamWrite>(server, 'GET', `/v1/writes/${first.proposal.ask}`, { token: DEVICE })).body;
    assert.equal(denied.state, 'not_sent');
    assert.deepEqual(sent(server), []);

    // Back to todo: upstream is still open, nothing to reopen. Done again, approved: sent once.
    await call(server, 'POST', `/v1/tasks/${task.id}/move`, { token: DEVICE, json: { to: 'todo' } });
    assert.equal((await writes(server)).length, 1);
    await call(server, 'POST', `/v1/tasks/${task.id}/move`, { token: DEVICE, json: { to: 'done' } });
    list = await writes(server, '?state=pending');
    const second = list[0];
    assert.ok(second);
    await answer(server, second.proposal.ask, 0);
    await call(server, 'POST', `/v1/integrations/${(await writes(server))[0]?.proposal.integration}/sync`, { token: DEVICE });
    assert.deepEqual(sent(server), [
      {
        method: 'PATCH',
        url: 'https://api.github.com/repos/example-org/demo-repo/issues/1',
        body: { state: 'closed', state_reason: 'completed' },
      },
    ]);
    const done = (await call<UpstreamWrite>(server, 'GET', `/v1/writes/${second.proposal.ask}`, { token: DEVICE })).body;
    assert.equal(done.state, 'sent');
    assert.equal(done.attempts, 1);
    assert.equal((await call(server, 'POST', `/v1/writes/${second.proposal.ask}/retry`, { token: DEVICE })).status, 409);
    // The mock's copy of upstream now has it closed, so a sync agrees: the task stays done.
    assert.equal(server.hub.findTaskById(task.id)?.status, 'done');
    const kinds = server.hub
      .eventsAfter(0)
      .filter((e: Event) => e.body.type.startsWith('write_'))
      .map((e: Event) => e.body.type);
    assert.deepEqual(kinds, ['write_proposed', 'write_finished', 'write_proposed', 'write_started', 'write_finished']);
  });
});

test('a created issue becomes the source, and a failure is retried once per retry', async () => {
  await withServer(async (server) => {
    await connect(server, IDEA, 'example-org/demo-repo#milestone:2');
    const created = await call<Task>(server, 'POST', '/v1/tasks', {
      token: DEVICE,
      json: { project: ID.paper, workstream: IDEA, title: 'Synthetic new issue', labels: ['docs'] },
    });
    assert.equal(created.status, 201);
    assert.deepEqual(await writes(server), [], 'creating a task creates no issue');
    const comment = { task: created.body.id, operation: 'comment', text: 'Hello' };
    assert.equal((await call(server, 'POST', '/v1/writes', { token: DEVICE, json: comment })).status, 409);
    assert.equal((await call(server, 'POST', '/v1/writes', { token: DEVICE, json: { task: created.body.id, operation: 'close' } })).status, 400);
    assert.equal((await call(server, 'POST', '/v1/writes', { token: DEVICE, json: { task: ID.writer, operation: 'create_issue' } })).status, 400);

    const create = await call<UpstreamWrite>(server, 'POST', '/v1/writes', {
      token: DEVICE,
      json: { task: created.body.id, operation: 'create_issue' },
    });
    assert.equal(create.status, 201);
    assert.equal(create.body.proposal.after.milestone, 'example-org/demo-repo#milestone:2');
    assert.deepEqual(sent(server), []);
    await answer(server, create.body.proposal.ask, 0);
    assert.equal(sent(server).length, 1);
    const task = server.hub.findTaskById(created.body.id);
    assert.equal(task?.source?.key, 'example-org/demo-repo#8');

    const posted = await call<UpstreamWrite>(server, 'POST', '/v1/writes', { token: DEVICE, json: comment });
    assert.equal(posted.status, 201);
    await answer(server, posted.body.proposal.ask, 0);
    const failed = (await call<UpstreamWrite>(server, 'GET', `/v1/writes/${posted.body.proposal.ask}`, { token: DEVICE })).body;
    assert.equal(failed.state, 'failed');
    assert.equal(failed.result?.outcome === 'failed' ? failed.result.status : undefined, 422);
    assert.equal(sent(server).length, 2);

    const retried = await call<UpstreamWrite>(server, 'POST', `/v1/writes/${posted.body.proposal.ask}/retry`, { token: DEVICE });
    assert.equal(retried.status, 202);
    assert.equal(sent(server).length, 3, 'a retry sends once more');
    const again = (await call<UpstreamWrite>(server, 'GET', `/v1/writes/${posted.body.proposal.ask}`, { token: DEVICE })).body;
    assert.equal(again.attempts, 2);
  });
});

test('an approval ask an agent raises itself sends nothing', async () => {
  await withServer(async (server) => {
    await connect(server, SEED_RUNS, 'example-org/demo-repo#milestone:1');
    const raised = await call<Ask>(server, 'POST', '/v1/asks', {
      token: AGENT,
      json: { kind: 'approval', to: ID.sam, title: 'GitHub: close example-org/demo-repo#1', options: ['Send', "Don't send"] },
    });
    assert.equal(raised.status, 201);
    assert.equal((await answer(server, raised.body.id, 0)).status, 200);
    assert.deepEqual(sent(server), []);
    assert.deepEqual(await writes(server), []);
  });
});
