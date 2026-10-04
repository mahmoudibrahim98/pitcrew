// API v1 "Integrations" and "Linking a workstream upstream", on both targets. The daemon reads
// the recorded fixtures in apps/mock-hub/fixtures (`--integration-fixtures`) and a stand-in `gh`
// first on its PATH; the mock reads the same fixtures. Nothing reaches GitHub or Jira.
import assert from 'node:assert/strict';
import { test } from 'node:test';
import { setTimeout as delay } from 'node:timers/promises';
import { list, schemas } from './schema.mjs';

const base = process.env.PITCREW_CONFORMANCE_URL;
const person = process.env.PITCREW_CONFORMANCE_PERSON;
const agent = process.env.PITCREW_CONFORMANCE_AGENT;
assert.ok(base && person && agent, 'Set PITCREW_CONFORMANCE_URL, _PERSON and _AGENT');
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
