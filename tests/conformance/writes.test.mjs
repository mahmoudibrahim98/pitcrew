// API v1 "Outward writes: every one approved first", on both targets, over the recorded fixtures
// in apps/mock-hub/fixtures (the daemon reads them with `--integration-fixtures`; its `gh` is the
// runner's stand-in). Nothing reaches GitHub. Writes run in the background on the daemon, so the
// suite polls for each outcome.
import assert from 'node:assert/strict';
import { test } from 'node:test';
import { setTimeout as delay } from 'node:timers/promises';
import { list, schemas } from './schema.mjs';

const base = process.env.PITCREW_CONFORMANCE_URL;
const person = process.env.PITCREW_CONFORMANCE_PERSON;
const agent = process.env.PITCREW_CONFORMANCE_AGENT;
const second = process.env.PITCREW_CONFORMANCE_SECOND_PERSON;
assert.ok(base && person && agent, 'Set PITCREW_CONFORMANCE_URL, _PERSON and _AGENT');
const missing = '01J00000000000000000000000';

async function call(path, { method = 'GET', body, token = person } = {}) {
  const response = await fetch(new URL(path, base), {
    method,
    headers: {
      ...(token ? { Authorization: `Bearer ${token}` } : {}),
      ...(body !== undefined ? { 'Content-Type': 'application/json' } : {}),
    },
    body: body === undefined ? undefined : JSON.stringify(body),
    signal: AbortSignal.timeout(10000),
  });
  const text = await response.text();
  const data = text ? JSON.parse(text) : undefined;
  if (response.status >= 400) schemas.error(data);
  return { status: response.status, data };
}
async function expect(status, path, options) {
  const reply = await call(path, options);
  assert.equal(reply.status, status, `${options?.method ?? 'GET'} ${path}: ${JSON.stringify(reply.data)}`);
  return reply.data;
}
/** Polls the write until `done` says so. */
async function until(id, done, what) {
  let write;
  for (const end = Date.now() + 60000; Date.now() < end; await delay(200)) {
    write = await expect(200, `/v1/writes/${id}`);
    if (done(write)) {
      schemas.upstreamWrite(write);
      return write;
    }
  }
  assert.fail(`${what}: ${JSON.stringify(write)}`);
}
async function pending(task) {
  for (const end = Date.now() + 60000; Date.now() < end; await delay(200)) {
    const found = await expect(200, `/v1/writes?task=${task}&state=pending`);
    if (found.length > 0) return found;
  }
  assert.fail('no write was proposed');
}
const answer = (ask, option) => expect(200, `/v1/asks/${ask}/answer`, { method: 'POST', body: { option } });

test('outward writes: approved first, sent once, results recorded', { timeout: 180000 }, async () => {
  // Who may call.
  for (const [method, path, body] of [
    ['GET', '/v1/writes'],
    ['POST', '/v1/writes', { task: missing, operation: 'comment', text: 'x' }],
    ['GET', `/v1/writes/${missing}`],
    ['POST', `/v1/writes/${missing}/retry`],
  ]) {
    await expect(403, path, { method, body, token: agent });
  }
  await expect(404, `/v1/writes/${missing}`);
  await expect(404, `/v1/writes/${missing}/retry`, { method: 'POST' });
  await expect(400, '/v1/writes?state=nope');
  await expect(400, '/v1/writes', { method: 'POST', body: { task: missing, operation: 'create_issue' } });

  // A GitHub connection, and a workstream of its own linked to milestone 2 (no issue of the
  // fixtures is in it, so the sync brings no task in).
  const integration = await expect(201, '/v1/integrations', {
    method: 'POST',
    body: { name: 'Synthetic writes', settings: { kind: 'github', repos: ['example-org/demo-repo'] }, credential: 'gh_cli' },
  });
  const key = `WR${Date.now().toString(36).toUpperCase().slice(-6)}`.replace(/[^A-Z0-9]/g, 'X').slice(0, 10);
  const project = await expect(201, '/v1/projects', { method: 'POST', body: { key, name: 'Synthetic writes project' } });
  const workstream = await expect(201, '/v1/workstreams', { method: 'POST', body: { project: project.id, name: 'Synthetic writes' } });
  const link = { system: 'github', key: 'example-org/demo-repo#milestone:2' };
  await expect(200, `/v1/workstreams/${workstream.id}`, { method: 'PATCH', body: { external: [link] } });
  await expect(202, `/v1/integrations/${integration.id}/sync`, { method: 'POST' });
  const task = await expect(201, '/v1/tasks', {
    method: 'POST',
    body: { project: project.id, workstream: workstream.id, title: 'Synthetic new issue', labels: ['docs'] },
  });
  assert.deepEqual(await expect(200, `/v1/writes?task=${task.id}`), [], 'creating a task creates no issue');

  // A comment needs an issue; operations the hub proposes itself cannot be asked for.
  await expect(409, '/v1/writes', { method: 'POST', body: { task: task.id, operation: 'comment', text: 'Hello' } });
  await expect(400, '/v1/writes', { method: 'POST', body: { task: task.id, operation: 'close' } });
  await expect(400, '/v1/writes', { method: 'POST', body: { task: task.id, operation: 'create_issue', text: 'x' } });

  // Asked for, then denied: not sent.
  const denied = await expect(201, '/v1/writes', { method: 'POST', body: { task: task.id, operation: 'create_issue' } });
  schemas.upstreamWrite(denied);
  assert.equal(denied.state, 'pending');
  assert.equal(denied.proposal.operation, 'create_issue');
  assert.deepEqual(denied.proposal.after, {
    title: 'Synthetic new issue',
    body: '',
    labels: ['docs'],
    milestone: 'example-org/demo-repo#milestone:2',
  });
  const asks = await expect(200, '/v1/asks?state=open');
  const ask = asks.find((a) => a.id === denied.proposal.ask);
  assert.ok(ask, 'the approval ask is open');
  schemas.ask(ask);
  assert.equal(ask.kind, 'approval');
  assert.equal(ask.task, task.id);
  assert.deepEqual(ask.options, ['Send', "Don't send"]);
  await answer(denied.proposal.ask, 1);
  const notSent = await until(denied.proposal.ask, (w) => w.state === 'not_sent', 'the denial was not recorded');
  assert.equal(notSent.result.outcome, 'not_sent');
  assert.equal(notSent.attempts, 0);

  // Asked again, approved: sent once, and the new issue becomes the task's source.
  const create = await expect(201, '/v1/writes', { method: 'POST', body: { task: task.id, operation: 'create_issue' } });
  await answer(create.proposal.ask, 0);
  const createdWrite = await until(create.proposal.ask, (w) => w.state === 'sent', 'the issue was not created');
  assert.equal(createdWrite.attempts, 1);
  assert.equal(createdWrite.result.created.key, 'example-org/demo-repo#8');
  const mirrored = await expect(200, `/v1/tasks/${task.id}`);
  assert.equal(mirrored.source.key, 'example-org/demo-repo#8');
  await expect(409, `/v1/writes/${create.proposal.ask}/retry`, { method: 'POST' });
  await expect(409, '/v1/writes', { method: 'POST', body: { task: task.id, operation: 'create_issue' } });

  // A person closes the task: the hub proposes closing the issue, and sends it once approved.
  await expect(200, `/v1/tasks/${task.id}/move`, { method: 'POST', body: { to: 'done' } });
  const [close] = await pending(task.id);
  assert.equal(close.proposal.operation, 'close');
  assert.deepEqual(close.proposal.after, { state: 'closed', close_reason: 'completed' });
  assert.equal(close.proposal.target.key, 'example-org/demo-repo#8');
  if (second) await expect(403, `/v1/writes/${close.proposal.ask}/retry`, { method: 'POST', token: second });
  await answer(close.proposal.ask, 0);
  await until(close.proposal.ask, (w) => w.state === 'sent', 'the close was not sent');

  // A comment upstream refuses (the fixture answers 422): failed; a retry sends it once more.
  const comment = await expect(201, '/v1/writes', {
    method: 'POST',
    body: { task: task.id, operation: 'comment', text: 'Synthetic comment.' },
  });
  assert.deepEqual(comment.proposal.after, { comment: 'Synthetic comment.' });
  await answer(comment.proposal.ask, 0);
  const failed = await until(comment.proposal.ask, (w) => w.state === 'failed', 'the comment did not fail');
  assert.equal(failed.result.status, 422);
  assert.equal(failed.attempts, 1);
  const retried = await expect(202, `/v1/writes/${comment.proposal.ask}/retry`, { method: 'POST' });
  schemas.upstreamWrite(retried);
  await until(comment.proposal.ask, (w) => w.attempts === 2 && w.state === 'failed', 'the retry was not sent');

  // The task's writes, and their events in its activity.
  const all = await expect(200, `/v1/writes?task=${task.id}`);
  list(schemas.upstreamWrite)(all);
  assert.deepEqual(
    all.map((w) => [w.proposal.operation, w.state]),
    [
      ['create_issue', 'not_sent'],
      ['create_issue', 'sent'],
      ['close', 'sent'],
      ['comment', 'failed'],
    ],
  );
  const failedOnly = await expect(200, `/v1/writes?task=${task.id}&state=failed&state=not_sent`);
  assert.deepEqual(failedOnly.map((w) => w.state), ['not_sent', 'failed']);
  const events = await expect(200, `/v1/events?limit=200`);
  schemas.events(events);
  const kinds = events.events
    .filter((e) => e.body.type.startsWith('write_') && (e.body.data.task ?? e.body.data.write?.task) === task.id)
    .map((e) => e.body.type);
  assert.deepEqual(kinds, [
    'write_proposed',
    'write_finished',
    'write_proposed',
    'write_started',
    'write_finished',
    'write_proposed',
    'write_started',
    'write_finished',
    'write_proposed',
    'write_started',
    'write_finished',
    'write_started',
    'write_finished',
  ]);

  // An approval an agent raises itself sends nothing.
  const crafted = await expect(201, '/v1/asks', {
    method: 'POST',
    token: agent,
    body: { kind: 'approval', to: integration.added_by, title: 'GitHub: close example-org/demo-repo#1', options: ['Send', "Don't send"] },
  });
  await answer(crafted.id, 0);
  await expect(404, `/v1/writes/${crafted.id}`);

  await expect(204, `/v1/integrations/${integration.id}`, { method: 'DELETE' });
  await expect(200, `/v1/workstreams/${workstream.id}`, { method: 'PATCH', body: { external: [] } });
});
