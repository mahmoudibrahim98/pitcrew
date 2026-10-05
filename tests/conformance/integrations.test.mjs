// API v1 "Integrations" and "Linking a workstream upstream", on both targets. Both read the
// runner's copy of the recorded fixtures in apps/mock-hub/fixtures (PITCREW_CONFORMANCE_FIXTURES;
// the daemon through `--integration-fixtures`, with a stand-in `gh` first on its PATH), again at
// each sync, so this test changes what upstream says by adding a file that sorts first. Nothing
// reaches GitHub or Jira.
import assert from 'node:assert/strict';
import { randomBytes } from 'node:crypto';
import { readFileSync, renameSync, rmSync, writeFileSync } from 'node:fs';
import { join } from 'node:path';
import { request as httpRequest } from 'node:http';
import { test } from 'node:test';
import { setTimeout as delay } from 'node:timers/promises';
import { list, schemas } from './schema.mjs';

const base = process.env.PITCREW_CONFORMANCE_URL;
const person = process.env.PITCREW_CONFORMANCE_PERSON;
const agent = process.env.PITCREW_CONFORMANCE_AGENT;
const fixtures = process.env.PITCREW_CONFORMANCE_FIXTURES;
assert.ok(base && person && agent && fixtures, 'Set PITCREW_CONFORMANCE_URL, _PERSON, _AGENT and _FIXTURES');
const missing = '01J00000000000000000000000';
const SECRET = 'synthetic-conformance-secret';

/** Every answer's body, to show no credential is ever in one. */
let seen = '';
async function call(path, { method = 'GET', body, token = person, raw } = {}) {
  const response = await fetch(new URL(path, base), {
    method,
    headers: {
      ...(token ? { Authorization: `Bearer ${token}` } : {}),
      ...(body !== undefined || raw !== undefined ? { 'Content-Type': 'application/json' } : {}),
    },
    body: raw ?? (body === undefined ? undefined : JSON.stringify(body)),
    signal: AbortSignal.timeout(10000),
  });
  const text = await response.text();
  seen += text;
  const data = text ? JSON.parse(text) : undefined;
  if (response.status >= 400) {
    schemas.error(data);
  }
  return { status: response.status, data };
}
async function expect(status, path, options) {
  const reply = await call(path, options);
  assert.equal(reply.status, status, `${options?.method ?? 'GET'} ${path}: ${JSON.stringify(reply.data)}`);
  return reply.data;
}

const github = {
  name: 'Demo repository',
  settings: { kind: 'github', repos: ['example-org/demo-repo'] },
  credential: 'gh_cli',
};
/** Every event of an activity query, newest page first, until `at_start` (bounded). */
async function activity(query, token = person) {
  const events = [];
  let before;
  for (let page = 0; page < 50; page++) {
    const params = new URLSearchParams(query);
    if (before !== undefined) params.set('before', String(before));
    const reply = await expect(200, `/v1/activity?${params}`, { token });
    schemas.events(reply);
    events.unshift(...reply.events);
    if (reply.at_start) return events;
    before = reply.from_rev;
  }
  throw new Error(`activity ${query} did not reach its start`);
}

/** The status of a refused `/v1/stream` handshake with `token` (an upgrade that succeeds is 101). */
function streamHandshake(token) {
  return new Promise((resolve, reject) => {
    const req = httpRequest(new URL('/v1/stream', base), {
      headers: {
        Connection: 'Upgrade',
        Upgrade: 'websocket',
        'Sec-WebSocket-Version': '13',
        'Sec-WebSocket-Key': randomBytes(16).toString('base64'),
        'Sec-WebSocket-Protocol': `pitcrew.v1, pitcrew.bearer.${token}`,
      },
    });
    req.setTimeout(5000, () => req.destroy(new Error('Upgrade timeout')));
    req.on('error', reject);
    req.on('upgrade', (res, socket) => {
      socket.destroy();
      resolve(res.statusCode);
    });
    req.on('response', (res) => {
      res.resume();
      res.on('end', () => resolve(res.statusCode));
    });
    req.end();
  });
}

/**
 * The events `/v1/stream` replays after `since` for the person, read until `done(events)` holds.
 * Hidden revisions leave gaps between frames, so this does not wait for the log's last revision.
 */
async function replay(since, done) {
  const url = new URL('/v1/stream', base);
  url.protocol = 'ws:';
  url.searchParams.set('since', String(since));
  const ws = new WebSocket(url, ['pitcrew.v1', `pitcrew.bearer.${person}`]);
  const events = [];
  try {
    return await new Promise((resolve, reject) => {
      const timer = setTimeout(() => reject(new Error(`stream replay: ${JSON.stringify(events.map((e) => e.body.type))}`)), 15000);
      ws.addEventListener('error', () => {
        clearTimeout(timer);
        reject(new Error('Stream failed'));
      });
      ws.addEventListener('message', (message) => {
        const frame = JSON.parse(message.data);
        if (frame.type !== 'events') return;
        assert.ok(frame.from_rev > since && frame.to_rev >= frame.from_rev);
        events.push(...frame.events);
        if (done(events)) {
          clearTimeout(timer);
          resolve(events);
        }
      });
    });
  } finally {
    ws.close();
  }
}

/** Whether an event names `session` in a `session` field, at any depth (the visibility rule). */
function namesSession(value, session) {
  if (Array.isArray(value)) return value.some((v) => namesSession(v, session));
  if (value === null || typeof value !== 'object') return false;
  return Object.entries(value).some(([key, v]) =>
    (key === 'session' && (v === session || v?.id === session)) || namesSession(v, session));
}

const ISSUES = 'https://api.github.com/repos/example-org/demo-repo/issues?state=all&sort=updated&direction=asc&per_page=100';
const MILESTONES = 'https://api.github.com/repos/example-org/demo-repo/milestones?state=all&sort=due_on&direction=asc&per_page=100';
const changed = [];

/** The recorded answer's body for `url`, as JSON. */
function recorded(url) {
  const block = readFileSync(join(fixtures, 'github.fixture'), 'utf8')
    .split('### pitcrew-github-fixture ###')
    .find((b) => b.trim().startsWith(`GET ${url} `));
  assert.ok(block, url);
  return JSON.parse(block.trim().split('\n\n').slice(2).join('\n\n'));
}

/** From now on, upstream answers `body` for `url`: a fixture file that sorts first, written whole. */
function upstreamSays(url, body) {
  const name = join(fixtures, url.includes('/milestones') ? '0-milestones.fixture' : '0-issues.fixture');
  writeFileSync(`${name}.tmp`, `GET ${url} HTTP/1.1\nAccept: application/vnd.github+json\n\nHTTP/1.1 200\n\n${JSON.stringify(body)}\n`);
  renameSync(`${name}.tmp`, name);
  changed.push(name);
}

/** `issues` with `change` made to issue `number`, updated later than recorded. */
function issuesWith(issues, number, change) {
  const copy = structuredClone(issues);
  const issue = copy.find((i) => i.number === number);
  change(issue);
  issue.updated_at = '2026-01-03T09:00:00Z';
  return copy;
}

const jira = {
  name: 'Demo Jira',
  settings: { kind: 'jira', deployment: 'cloud', site: 'https://jira.example.com', projects: ['DEMO'], email: 'sam@example.com' },
  credential: 'stored',
};

test('integrations: device-only routes, checks, credentials, links and a sync', { timeout: 120000 }, async () => {
  // Who may call.
  await expect(401, '/v1/integrations', { token: null });
  for (const [method, path, body] of [
    ['GET', '/v1/integrations'],
    ['POST', '/v1/integrations', github],
    ['GET', `/v1/integrations/${missing}`],
    ['DELETE', `/v1/integrations/${missing}`],
    ['POST', `/v1/integrations/${missing}/test`],
    ['POST', `/v1/integrations/${missing}/sync`],
    ['PUT', `/v1/integrations/${missing}/credential`, { secret: SECRET }],
  ]) {
    await expect(403, path, { method, body, token: agent });
  }
  for (const [method, path, body] of [
    ['GET', `/v1/integrations/${missing}`],
    ['DELETE', `/v1/integrations/${missing}`],
    ['POST', `/v1/integrations/${missing}/test`],
    ['POST', `/v1/integrations/${missing}/sync`],
    ['PUT', `/v1/integrations/${missing}/credential`, { secret: SECRET }],
  ]) {
    await expect(404, path, { method, body });
  }

  // Malformed connections.
  for (const bad of [
    { ...github, name: '   ' },
    { ...github, credential: 'password' },
    { ...github, settings: { kind: 'github', repos: [] } },
    { ...github, settings: { kind: 'github', repos: ['not a repo'] } },
    { ...github, settings: { kind: 'github', repos: ['example-org/demo-repo'], api_base: 'http://ghe.example.com/api/v3' } },
    { ...github, settings: { kind: 'gitlab', repos: ['example-org/demo-repo'] } },
    { ...github, interval_minutes: 4 },
    { ...jira, credential: 'gh_cli' },
    { ...jira, settings: { ...jira.settings, email: undefined } },
    { ...jira, settings: { ...jira.settings, projects: ['DEMO" OR 1=1'] } },
  ]) {
    await expect(400, '/v1/integrations', { method: 'POST', body: bad });
  }
  await expect(400, '/v1/integrations', { method: 'POST', raw: '{' });

  // GitHub through `gh auth token`; one connection per repository.
  const added = await expect(201, '/v1/integrations', { method: 'POST', body: github });
  schemas.integration(added);
  assert.deepEqual(added.credential, { source: 'gh_cli', stored: false });
  assert.equal(added.interval_minutes, 15);
  await expect(409, '/v1/integrations', { method: 'POST', body: github });
  await expect(409, `/v1/integrations/${added.id}/credential`, { method: 'PUT', body: { secret: SECRET } });

  // Jira with a stored secret, which never comes back.
  const withSecret = await expect(201, '/v1/integrations', { method: 'POST', body: jira });
  const credential = `/v1/integrations/${withSecret.id}/credential`;
  for (const bad of [{ secret: '' }, { secret: 'two words' }, { secret: 42 }, {}]) {
    await expect(400, credential, { method: 'PUT', body: bad });
  }
  await expect(204, credential, { method: 'PUT', body: { secret: SECRET } });
  const kept = await expect(200, `/v1/integrations/${withSecret.id}`);
  assert.deepEqual(kept.credential, { source: 'stored', stored: true });
  const jiraCheck = await expect(200, `/v1/integrations/${withSecret.id}/test`, { method: 'POST' });
  schemas.integrationCheck(jiraCheck);
  assert.equal(jiraCheck.ok, true, JSON.stringify(jiraCheck));

  // A workstream of its own, linked to milestone 1.
  const projects = await expect(200, '/v1/projects');
  const key = `IG${Date.now().toString(36).toUpperCase().slice(-6)}`.replace(/[^A-Z0-9]/g, 'X').slice(0, 10);
  const project = await expect(201, '/v1/projects', { method: 'POST', body: { key, name: 'Synthetic integration project' } });
  assert.ok(projects.every((p) => p.id !== project.id));
  const workstream = await expect(201, '/v1/workstreams', { method: 'POST', body: { project: project.id, name: 'Synthetic launch' } });
  const link = { system: 'github', key: 'example-org/demo-repo#milestone:1', url: 'https://github.com/example-org/demo-repo/milestone/1' };
  await expect(403, `/v1/workstreams/${workstream.id}`, { method: 'PATCH', body: { external: [link] }, token: agent });
  for (const external of [
    [{ ...link, url: 'http://github.com/example-org/demo-repo/milestone/1' }],
    [{ ...link, url: 'https://user:pass@github.com/example-org/demo-repo' }],
    [link, link],
    [{ system: 'github', key: '' }],
    'not a list',
  ]) {
    await expect(400, `/v1/workstreams/${workstream.id}`, { method: 'PATCH', body: { external } });
  }
  // The log's head before the link: the stream is replayed from here below.
  const head = (await expect(200, '/v1/events?limit=1')).to_rev;
  const linked = await expect(200, `/v1/workstreams/${workstream.id}`, { method: 'PATCH', body: { external: [link] } });
  schemas.workstream(linked);
  assert.deepEqual(linked.external, [link]);
  const events = await expect(200, `/v1/events?limit=50`);
  assert.ok(events.events.some((e) => e.body.type === 'workstream_linked' && e.body.data.workstream === workstream.id));

  // Sync now: issue #1 (open, milestone 1) becomes a task of that workstream; #3 (in milestone 1,
  // closed before it was first seen) does not.
  schemas.integration(await expect(202, `/v1/integrations/${added.id}/sync`, { method: 'POST' }));
  let tasks = [];
  for (const until = Date.now() + 60000; Date.now() < until; await delay(200)) {
    tasks = await expect(200, `/v1/tasks?workstream=${workstream.id}`);
    if (tasks.length > 0) break;
  }
  assert.deepEqual(tasks.map((t) => t.source?.key), ['example-org/demo-repo#1']);
  list(schemas.task)(tasks);
  assert.equal(tasks[0].title, 'Fix flaky login test');
  assert.equal(tasks[0].status, 'todo');
  assert.equal(tasks[0].project, project.id);
  let synced;
  for (const until = Date.now() + 60000; Date.now() < until; await delay(200)) {
    synced = await expect(200, `/v1/integrations/${added.id}`);
    if (!synced.status.running && synced.status.last_success_at !== undefined && synced.links.length > 0) break;
  }
  schemas.integration(synced);
  assert.deepEqual(synced.status.problems, []);
  assert.ok(synced.links.some((l) => l.workstream === workstream.id && l.scope.key === link.key && l.title === 'v1 launch'));

  // What the link and the sync wrote reaches activity and the stream only through the hub's
  // shared visibility check (api-v1.md, "Session import"). An agent token reads neither. With
  // every session excluded, the sync's events stay visible (they name no session: non-session
  // work), while an excluded session's events are hidden from the same answers.
  const createdBySync = (e) => e.body.type === 'task_created' && e.body.data.task.id === tasks[0].id;
  const linkedHere = (e) => e.body.type === 'workstream_linked' && e.body.data.workstream === workstream.id;
  await expect(403, '/v1/activity', { token: agent });
  await expect(403, `/v1/activity?workstream=${workstream.id}`, { token: agent });
  assert.equal(await streamHandshake(agent), 403);
  // A seeded session with activity of its own, to show the same answers hide it.
  let excluded;
  for (const session of await expect(200, '/v1/sessions')) {
    if ((await activity(`session=${session.id}`)).length > 0) {
      excluded = session.id;
      break;
    }
  }
  assert.ok(excluded, 'Use a fresh seeded demo server');
  const syncMember = (await activity(`workstream=${workstream.id}`)).find(createdBySync)?.author;
  assert.ok(syncMember, 'the sync created its task');
  await expect(200, '/v1/import', { method: 'PUT', body: { mode: 'none' } });
  try {
    await expect(404, `/v1/sessions/${excluded}`);
    const here = await activity(`workstream=${workstream.id}`);
    assert.ok(here.some(createdBySync) && here.some(linkedHere), JSON.stringify(here.map((e) => e.body.type)));
    const newest = await expect(200, '/v1/activity?limit=200');
    assert.ok(newest.events.some(createdBySync));
    assert.ok(!newest.events.some((e) => namesSession(e.body, excluded)));
    assert.deepEqual(await activity(`session=${excluded}`), []);
    const replayed = await replay(head, (events) => events.some(createdBySync));
    assert.ok(replayed.some(linkedHere));
    assert.ok(replayed.filter(createdBySync).every((e) => e.author === syncMember));
    assert.ok(!replayed.some((e) => namesSession(e.body, excluded)));
  } finally {
    await expect(200, '/v1/import', { method: 'PUT', body: { mode: 'all' } });
  }
  await expect(200, `/v1/sessions/${excluded}`);

  // Upstream changes, applied alike by both targets ("What a sync does"): only a change acts.
  /** Syncs now and waits for that sync to end. */
  const syncNow = async () => {
    const asked = Date.now();
    await expect(202, `/v1/integrations/${added.id}/sync`, { method: 'POST' });
    for (const until = Date.now() + 60000; Date.now() < until; await delay(100)) {
      const now = await expect(200, `/v1/integrations/${added.id}`);
      if (!now.status.running && (now.status.last_attempt_at ?? 0) >= asked) {
        assert.deepEqual(now.status.problems, []);
        return;
      }
    }
    throw new Error('the sync did not end');
  };
  const byKey = async () => new Map((await expect(200, `/v1/tasks?workstream=${workstream.id}`)).map((t) => [t.source?.key, t]));
  try {
    // An open issue moved into the linked milestone becomes a task there; a closed one does not.
    const milestone = { number: 1, title: 'v1 launch', state: 'open', html_url: 'https://github.com/example-org/demo-repo/milestone/1' };
    let issues = issuesWith(recorded(ISSUES), 4, (i) => (i.milestone = milestone));
    issues = issuesWith(issues, 2, (i) => (i.milestone = milestone));
    upstreamSays(ISSUES, issues);
    await syncNow();
    let mirrored = await byKey();
    assert.deepEqual([...mirrored.keys()].sort(), ['example-org/demo-repo#1', 'example-org/demo-repo#4']);
    assert.equal(mirrored.get('example-org/demo-repo#4').title, 'Write the release notes');
    assert.equal(mirrored.get('example-org/demo-repo#4').status, 'todo');

    // Upstream closes #1 and the milestone: the task is done and the workstream ships.
    upstreamSays(ISSUES, issuesWith(issues, 1, (i) => {
      i.state = 'closed';
      i.state_reason = 'completed';
    }));
    upstreamSays(MILESTONES, recorded(MILESTONES).map((m) => (m.number === 1 ? { ...m, state: 'closed' } : m)));
    await syncNow();
    mirrored = await byKey();
    const one = mirrored.get('example-org/demo-repo#1');
    assert.equal(one.status, 'done');
    assert.equal((await expect(200, `/v1/workstreams/${workstream.id}`)).status, 'shipped');

    // A person reopens the task: with nothing new upstream, the next sync leaves it there.
    await expect(200, `/v1/tasks/${one.id}/move`, { method: 'POST', body: { to: 'todo' } });
    await syncNow();
    assert.equal((await byKey()).get('example-org/demo-repo#1').status, 'todo');
  } finally {
    for (const name of changed) rmSync(name, { force: true });
  }

  // A test reads once, and warns that this credential could write.
  const check = await expect(200, `/v1/integrations/${added.id}/test`, { method: 'POST' });
  schemas.integrationCheck(check);
  assert.equal(check.ok, true, JSON.stringify(check));
  assert.ok(check.warnings.length > 0);

  // The list holds both; removing one forgets it.
  const all = await expect(200, '/v1/integrations');
  list(schemas.integration)(all);
  assert.ok(all.some((i) => i.id === added.id) && all.some((i) => i.id === withSecret.id));
  await expect(204, `/v1/integrations/${withSecret.id}`, { method: 'DELETE' });
  await expect(404, `/v1/integrations/${withSecret.id}`);
  await expect(204, `/v1/integrations/${added.id}`, { method: 'DELETE' });
  // Unlink, so the workstream is as it was.
  await expect(200, `/v1/workstreams/${workstream.id}`, { method: 'PATCH', body: { external: [] } });

  assert.ok(!seen.includes(SECRET), 'an answer held the credential');
});
