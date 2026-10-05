// The Orchestrator (api-v1.md, "Orchestrator") and the reader token (api-v1.md, "Transport and
// auth"), against either target. Run alone, after the board drafts: a question starts an agent's
// CLI (a stand-in on the daemon, which never answers; the mock answers after its reply delay).
import assert from 'node:assert/strict';
import { readFileSync } from 'node:fs';
import { join } from 'node:path';
import { test } from 'node:test';
import { list, schemas } from './schema.mjs';

const base = process.env.PITCREW_CONFORMANCE_URL;
const person = process.env.PITCREW_CONFORMANCE_PERSON;
const agent = process.env.PITCREW_CONFORMANCE_AGENT;
const reader = process.env.PITCREW_CONFORMANCE_READER;
const second = process.env.PITCREW_CONFORMANCE_SECOND_PERSON;
assert.ok(base && person && agent && reader && second, 'Set PITCREW_CONFORMANCE_URL, _PERSON, _AGENT, _READER and _SECOND_PERSON');
assert.ok(['127.0.0.1', 'localhost', '[::1]'].includes(new URL(base).hostname));
const missing = '01J00000000000000000000000';
const codes = { 400: 'invalid', 403: 'forbidden', 404: 'not_found', 409: 'conflict' };

async function call(method, path, body, token = person, headers = {}) {
  const response = await fetch(base + path, {
    method,
    headers: { Authorization: `Bearer ${token}`, 'Content-Type': 'application/json', ...headers },
    ...(body === undefined ? {} : { body: typeof body === 'string' ? body : JSON.stringify(body) }),
    signal: AbortSignal.timeout(15000),
  });
  const text = await response.text();
  return { status: response.status, body: text === '' ? undefined : JSON.parse(text) };
}

async function expect(status, method, path, body, token, schema) {
  const response = await call(method, path, body, token);
  assert.equal(response.status, status, `${method} ${path}: ${JSON.stringify(response.body)}`);
  if (status >= 400) {
    schemas.error(response.body);
    assert.equal(response.body.code, codes[status]);
  } else if (schema) schema(response.body);
  return response.body;
}

/** Every route of the contract with a method that writes, its parameters filled in. */
function contractWrites() {
  const contract = readFileSync(join(import.meta.dirname, '../../docs/build/contracts/api-v1.md'), 'utf8');
  const routes = new Map();
  for (const piece of contract.split('`')) {
    const match = /^(POST|PUT|PATCH|DELETE) (\/v1\/\S*)$/.exec(piece);
    if (match === null) continue;
    const path = match[2]
      .split('?')[0]
      .replace('{project\\|workstream}', 'workstream')
      .split('/')
      .map((segment) =>
        segment === '{engine}' ? 'claude' : segment === '{event}' ? 'Stop' : segment === '{scope}' ? 'workspace' : segment.startsWith('{') ? missing : segment,
      )
      .join('/');
    routes.set(`${match[1]} ${path}`, [match[1], path]);
  }
  return [...routes.values()];
}

test('a reader token reads the routes marked read or agent, and is refused every write', async () => {
  const writes = contractWrites();
  assert.ok(writes.length > 30, `the contract's writes: ${writes.length}`);
  for (const [method, path] of writes) {
    await expect(403, method, path, {}, reader);
  }
  const machine = (await expect(200, 'GET', '/v1/machines', undefined, person))[0].id;
  for (const path of [
    '/v1/me/cursors',
    '/v1/safety',
    '/v1/import',
    '/v1/orchestrator',
    '/v1/board-drafts',
    // Integrations, outward writes and machine setup: device-only reads.
    '/v1/integrations',
    '/v1/writes',
    `/v1/machines/${machine}/check`,
    `/v1/machines/${machine}/agents`,
    `/v1/machines/${machine}/agents/claude/sign-in`,
  ]) {
    await expect(403, 'GET', path, undefined, reader);
  }
  const me = await expect(200, 'GET', '/v1/me', undefined, reader, schemas.member);
  assert.equal(me.kind, 'agent');
  for (const [path, schema] of [
    ['/v1/workspace', schemas.workspace],
    ['/v1/machines', list(schemas.machine)],
    ['/v1/projects', list(schemas.project)],
    ['/v1/workstreams', list(schemas.workstream)],
    ['/v1/tasks', list(schemas.task)],
    ['/v1/sessions', list(schemas.session)],
    ['/v1/events?limit=5', schemas.events],
    ['/v1/recaps/blocks?limit=5', schemas.blocks],
  ]) {
    await expect(200, 'GET', path, undefined, reader, schema);
  }
  const session = (await expect(200, 'GET', '/v1/sessions', undefined, reader))[0];
  await expect(200, 'GET', `/v1/sessions/${session.id}`, undefined, reader, schemas.session);
  await expect(403, 'GET', `/v1/sessions/${session.id}/transcript`, undefined, reader);
  // An agent cannot make the reads marked read.
  await expect(403, 'GET', '/v1/sessions', undefined, agent);
  // A reader's socket would carry input: refused.
  const socket = await fetch(`${base}/v1/stream`, {
    headers: {
      Authorization: `Bearer ${reader}`,
      Upgrade: 'websocket',
      Connection: 'Upgrade',
      'Sec-WebSocket-Version': '13',
      'Sec-WebSocket-Key': 'dGhlIHNhbXBsZSBub25jZQ==',
      'Sec-WebSocket-Protocol': 'pitcrew.v1',
    },
    signal: AbortSignal.timeout(15000),
  }).catch(() => undefined);
  if (socket !== undefined) {
    assert.notEqual(socket.status, 101);
    assert.ok([400, 403].includes(socket.status), `GET /v1/stream upgrade: ${socket.status}`);
    await socket.body?.cancel();
  }
});

test('the orchestrator: a person\'s own conversations, one answer at a time, cancel and clear', async () => {
  // People only.
  for (const token of [agent, reader]) {
    await expect(403, 'GET', '/v1/orchestrator', undefined, token);
    await expect(403, 'POST', '/v1/orchestrator/questions', { text: 'Synthetic?' }, token);
    await expect(403, 'POST', `/v1/orchestrator/conversations/${missing}/cancel`, undefined, token);
    await expect(403, 'DELETE', '/v1/orchestrator/conversations', undefined, token);
  }
  const before = await expect(200, 'GET', '/v1/orchestrator', undefined, person, schemas.orchestrator);
  assert.deepEqual(before.engines.map((e) => e.engine), ['claude', 'codex', 'opencode']);
  assert.equal(before.limits.question_chars, 4000);
  assert.equal(before.limits.answer_bytes, 16384);
  assert.equal(before.limits.answer_seconds, 300);
  assert.ok(before.engines[0].installed, 'Claude Code (a stand-in on the daemon) is installed');

  // What a question must be.
  for (const body of [{ text: ' \u0007 ' }, { text: 'x'.repeat(4001) }, {}, [1], { text: 'Synthetic?', engine: 'gpt' }]) {
    await expect(400, 'POST', '/v1/orchestrator/questions', body);
  }
  await expect(404, 'POST', '/v1/orchestrator/questions', { text: 'Synthetic?', conversation: missing });
  await expect(400, 'POST', '/v1/orchestrator/questions', { text: 'Synthetic?', agent: missing });
  const me = await expect(200, 'GET', '/v1/me', undefined, person, schemas.member);
  await expect(400, 'POST', '/v1/orchestrator/questions', { text: 'Synthetic?', agent: me.id });

  // A question: a session titled Orchestrator, its turn answering.
  const asked = await expect(202, 'POST', '/v1/orchestrator/questions', { text: 'What did my agents do today?', engine: 'claude' }, person, schemas.conversation);
  assert.equal(asked.engine, 'claude');
  assert.equal(asked.turns.length, 1);
  const turn = asked.turns[0];
  assert.equal(turn.question, 'What did my agents do today?');
  assert.equal(turn.state, 'answering');
  assert.equal(turn.usage, undefined);
  const session = await expect(200, 'GET', `/v1/sessions/${turn.session}`, undefined, person, schemas.session);
  assert.equal(session.title, 'Orchestrator');
  assert.equal(session.agent, asked.agent);
  assert.equal(session.workstream, undefined);
  const now = await expect(200, 'GET', '/v1/orchestrator', undefined, person, schemas.orchestrator);
  assert.equal(now.engine, 'claude');
  assert.equal(now.conversations[0].id, asked.id);

  // One answer at a time; another person sees none of it.
  const answering = now.conversations[0].turns[0].state === 'answering';
  if (answering) {
    await expect(409, 'POST', '/v1/orchestrator/questions', { text: 'Another?' });
  }
  const theirs = await expect(200, 'GET', '/v1/orchestrator', undefined, second, schemas.orchestrator);
  assert.deepEqual(theirs.conversations, []);
  await expect(404, 'POST', `/v1/orchestrator/conversations/${asked.id}/cancel`, undefined, second);
  await expect(404, 'POST', '/v1/orchestrator/questions', { text: 'Theirs?', conversation: asked.id }, second);
  await expect(404, 'POST', `/v1/orchestrator/conversations/${missing}/cancel`);

  // Cancel stops an answer under way (Esc); a second cancel finds none.
  if (answering) {
    const canceled = await expect(200, 'POST', `/v1/orchestrator/conversations/${asked.id}/cancel`, undefined, person, schemas.conversation);
    assert.ok(['canceled', 'answered'].includes(canceled.turns[0].state), canceled.turns[0].state);
  }
  await expect(409, 'POST', `/v1/orchestrator/conversations/cnv_${asked.id}/cancel`);
  const ended = (await expect(200, 'GET', '/v1/orchestrator', undefined, person, schemas.orchestrator)).conversations[0].turns[0];
  assert.notEqual(ended.state, 'answering');
  schemas.conversation({ id: asked.id, engine: 'claude', agent: asked.agent, started: asked.started, turns: [ended] });
  assert.ok(ended.usage !== undefined && Number.isSafeInteger(ended.ended));

  // Clear forgets the caller's conversations and ends their session; the engine stays.
  const cleared = await call('DELETE', '/v1/orchestrator/conversations');
  assert.equal(cleared.status, 204);
  const after = await expect(200, 'GET', '/v1/orchestrator', undefined, person, schemas.orchestrator);
  assert.deepEqual(after.conversations, []);
  assert.equal(after.engine, 'claude');
  const deadline = Date.now() + 15000;
  for (;;) {
    const state = (await expect(200, 'GET', `/v1/sessions/${turn.session}`, undefined, person)).state;
    if (state === 'ended') break;
    assert.ok(Date.now() < deadline, `the cleared session is still ${state}`);
    await new Promise((done) => setTimeout(done, 100));
  }
});
