// API v1 "Machine setup", against both targets: the machine check, the agents' accounts and a
// sign-in terminal, on the hub's own machine only and for its owner only (a device token, and not
// another person's: PITCREW_CONFORMANCE_SECOND_PERSON). Its own file, so the shared cases in
// api.test.mjs stay as they are.
//
// What the rows and accounts say depends on the machine (the mock's are synthetic; the daemon's
// come from the stand-in CLIs run.mjs puts first on its PATH, and the runner's own git, gh and
// tmux), so only their shape and the contract's rules are checked. A sign-in runs the stand-in's
// "login" (the daemon target) or a canned one (the mock): never a real CLI's.
import assert from 'node:assert/strict';
import { randomBytes } from 'node:crypto';
import { request as httpRequest } from 'node:http';
import { before, test } from 'node:test';
import { bool, enumeration, integer, list, object, schemas, text } from './schema.mjs';

const base = process.env.PITCREW_CONFORMANCE_URL;
const person = process.env.PITCREW_CONFORMANCE_PERSON;
const agent = process.env.PITCREW_CONFORMANCE_AGENT;
const second = process.env.PITCREW_CONFORMANCE_SECOND_PERSON;
assert.ok(base && person && agent && second, 'Set PITCREW_CONFORMANCE_URL, _PERSON, _AGENT and _SECOND_PERSON');
assert.ok(
  ['127.0.0.1', 'localhost', '[::1]'].includes(new URL(base).hostname),
  'Conformance changes synthetic local state only',
);
const missing = '01J00000000000000000000000';
const codes = { 400: 'invalid', 401: 'unauthorized', 403: 'forbidden', 404: 'not_found', 409: 'conflict' };

const engine = enumeration('claude', 'codex', 'opencode');
const item = enumeration(
  'cli_claude',
  'cli_codex',
  'cli_opencode',
  'tmux',
  'git',
  'gh',
  'disk',
  'slurm',
  'helper',
);
const row = object({
  id: item,
  status: enumeration('ok', 'warn', 'missing'),
  detail: text,
  'version?': text,
  'fix?': enumeration('install_page', 'install_helper'),
});
const check = object({ rows: list(row) });
const account = object({
  engine,
  installed: bool,
  'signed_in?': bool,
  'account?': text,
  'detail?': text,
});
const signIn = object({
  engine,
  terminal: (v) => {
    text(v);
    assert.match(v, /^[0-9A-HJKMNP-TV-Z]{26}$/);
  },
  command: list(text),
  running: bool,
  started: integer,
});

async function call(path, { method = 'GET', token = person, body } = {}) {
  const headers = token ? { Authorization: `Bearer ${token}` } : {};
  if (body !== undefined) headers['Content-Type'] = 'application/json';
  const response = await fetch(new URL(path, base), {
    method,
    headers,
    body: body === undefined ? undefined : JSON.stringify(body),
    signal: AbortSignal.timeout(60000),
  });
  const textBody = await response.text();
  return { status: response.status, body: textBody === '' ? undefined : JSON.parse(textBody) };
}

async function refused(path, status, options) {
  const reply = await call(path, options);
  assert.equal(reply.status, status, JSON.stringify(reply.body));
  schemas.error(reply.body);
  assert.equal(reply.body.code, codes[status]);
  assert.ok(reply.body.message.length);
}

function upgrade(path, token) {
  return new Promise((resolve, reject) => {
    const req = httpRequest(new URL(path, base), {
      headers: {
        Connection: 'Upgrade',
        Upgrade: 'websocket',
        'Sec-WebSocket-Version': '13',
        'Sec-WebSocket-Key': randomBytes(16).toString('base64'),
        'Sec-WebSocket-Protocol': `pitcrew.v1, pitcrew.bearer.${token}`,
      },
    });
    req.setTimeout(10000, () => req.destroy(new Error('Upgrade timeout')));
    req.on('error', reject);
    req.on('upgrade', (res, socket) => {
      socket.destroy();
      resolve({ status: res.statusCode, protocol: res.headers['sec-websocket-protocol'] });
    });
    req.on('response', (res) => {
      res.resume();
      res.on('end', () => resolve({ status: res.statusCode }));
    });
    req.end();
  });
}

let own, other;
before(async () => {
  const machines = (await call('/v1/machines')).body;
  own = machines.find((m) => m.kind === 'local');
  other = machines.find((m) => m.id !== own?.id);
  assert.ok(own && other, 'Use a seeded demo server: a local machine and another one');
});

test('machine check shape and order', { timeout: 60000 }, async () => {
  const reply = await call(`/v1/machines/${own.id}/check`);
  assert.equal(reply.status, 200, JSON.stringify(reply.body));
  check(reply.body);
  const order = ['cli_claude', 'cli_codex', 'cli_opencode', 'tmux', 'git', 'gh', 'disk', 'slurm', 'helper'];
  const ids = reply.body.rows.map((r) => r.id);
  assert.deepEqual(ids, [...ids].sort((a, b) => order.indexOf(a) - order.indexOf(b)), 'the fixed order');
  for (const required of ['cli_claude', 'cli_codex', 'cli_opencode', 'git', 'gh', 'disk'])
    assert.ok(ids.includes(required), `a ${required} row`);
  assert.ok(!ids.includes('helper'), "no helper row on the hub's own machine");
  for (const r of reply.body.rows) {
    if (r.status === 'ok') assert.equal(r.fix, undefined, `an ok row has no fix: ${r.id}`);
    if (r.status === 'missing' && r.id !== 'disk') assert.equal(r.fix, 'install_page', r.id);
  }
  const one = await call(`/v1/machines/${own.id}/check?row=git`);
  assert.equal(one.status, 200);
  check(one.body);
  assert.deepEqual(one.body.rows.map((r) => r.id), ['git']);
  await refused(`/v1/machines/${own.id}/check?row=cli-claude`, 400);
});

test('machine setup refusals', { timeout: 60000 }, async () => {
  for (const path of [`/v1/machines/${own.id}/check`, `/v1/machines/${own.id}/agents`]) {
    await refused(path, 401, { token: null });
    await refused(path, 401, { token: 'synthetic-invalid-token' });
    await refused(path, 403, { token: agent });
  }
  await refused(`/v1/machines/${missing}/check`, 404);
  await refused(`/v1/machines/${missing}/agents`, 404);
  await refused(`/v1/machines/${other.id}/check`, 409);
  await refused(`/v1/machines/${other.id}/agents`, 409);
  const signInPath = `/v1/machines/${own.id}/agents/claude/sign-in`;
  await refused(signInPath, 403, { method: 'POST', token: agent, body: {} });
  await refused(signInPath, 403, { token: agent });
  await refused(`/v1/machines/${other.id}/agents/claude/sign-in`, 409, { method: 'POST', body: {} });
  await refused(`/v1/machines/${own.id}/agents/gemini/sign-in`, 404, { method: 'POST', body: {} });
  await refused(signInPath, 400, { method: 'POST', body: { method: 'device_code' } });
  await refused(signInPath, 400, { method: 'POST', body: { method: 'password' } });
  await refused(signInPath, 400, { method: 'POST', body: { token: 'synthetic' } });
  await refused(signInPath, 403, { method: 'DELETE', token: agent });
});

test('machine setup is the hub owner’s only', { timeout: 60000 }, async () => {
  // Another person's device token: refused on every route, before anything else is looked at.
  const signInPath = `/v1/machines/${own.id}/agents/claude/sign-in`;
  for (const [method, path] of [
    ['GET', `/v1/machines/${own.id}/check`],
    ['GET', `/v1/machines/${own.id}/agents`],
    ['GET', signInPath],
    ['POST', signInPath],
    ['DELETE', signInPath],
    ['GET', `/v1/machines/${other.id}/check`],
    ['POST', `/v1/machines/${own.id}/agents/gemini/sign-in`],
  ])
    await refused(path, 403, { method, token: second, body: method === 'POST' ? {} : undefined });
  // Their token itself works.
  assert.equal((await call('/v1/machines', { token: second })).status, 200);
});

test('agents accounts shape', { timeout: 60000 }, async () => {
  const reply = await call(`/v1/machines/${own.id}/agents`);
  assert.equal(reply.status, 200, JSON.stringify(reply.body));
  list(account)(reply.body);
  assert.deepEqual(
    reply.body.map((a) => a.engine),
    ['claude', 'codex', 'opencode'],
  );
  for (const a of reply.body) {
    if (!a.installed) assert.equal(a.signed_in, undefined, `${a.engine}: not installed, so not known`);
    if (a.signed_in === undefined) assert.ok(a.detail, `${a.engine}: why it is not known`);
    if (a.signed_in !== true) assert.equal(a.account, undefined, `${a.engine}: no account`);
  }
});

test('sign-in terminal', { timeout: 60000 }, async () => {
  const path = `/v1/machines/${own.id}/agents/codex/sign-in`;
  const started = await call(path, { method: 'POST' });
  assert.ok([200, 201].includes(started.status), JSON.stringify(started.body));
  signIn(started.body);
  assert.equal(started.body.engine, 'codex');
  assert.deepEqual(started.body.command, ['codex', 'login']);
  const status = await call(path);
  assert.equal(status.status, 200);
  signIn(status.body);
  assert.equal(status.body.terminal, started.body.terminal);
  if (status.body.running) {
    const again = await call(path, { method: 'POST', body: {} });
    assert.equal(again.status, 200, 'one per CLI while it runs');
    assert.equal(again.body.terminal, started.body.terminal);
  }
  // Not a session; the terminals route serves it to a person only.
  await refused(`/v1/sessions/${started.body.terminal}`, 404);
  const terminal = `/v1/sessions/${started.body.terminal}/terminal?cols=80&rows=24`;
  assert.equal((await upgrade(terminal, agent)).status, 403);
  assert.equal((await upgrade(terminal, second)).status, 403, 'only for the person who started it');
  // However its id is written in the path (`ses%5F…`, an encoded character of its ULID).
  const id = started.body.terminal.startsWith('ses_') ? started.body.terminal.slice(4) : started.body.terminal;
  const hex = (c) => c.charCodeAt(0).toString(16).toUpperCase().padStart(2, '0');
  for (const form of [`ses%5F${id}`, `ses%5f${id}`, `ses_%${hex(id[0])}${id.slice(1)}`, `%${hex(id[0])}${id.slice(1)}`]) {
    const status = (await upgrade(`/v1/sessions/${form}/terminal?cols=80&rows=24`, second)).status;
    assert.equal(status, 403, `only for the person who started it, as ${form}`);
  }
  const opened = await upgrade(terminal, person);
  assert.equal(opened.status, 101);
  assert.equal(opened.protocol, 'pitcrew.v1');

  // Leaving it stops it: its terminal is gone.
  const stopped = await call(path, { method: 'DELETE' });
  assert.equal(stopped.status, 204, JSON.stringify(stopped.body));
  assert.equal(stopped.body, undefined);
  await refused(path, 404);
  await refused(path, 404, { method: 'DELETE' });
  assert.equal((await upgrade(terminal, person)).status, 404);
});
