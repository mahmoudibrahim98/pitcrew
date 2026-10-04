// GitHub and Jira integrations (api-v1.md, "Integrations"): device-only routes, links on
// workstreams, a sync over the recorded fixtures, and credentials that never come back.

import assert from 'node:assert/strict';
import { test } from 'node:test';
import { AGENT, DEVICE, ID, call, withServer } from './helpers.ts';
import type { Integration, IntegrationCheck, Task, Workstream } from '../src/types.ts';

const SEED_RUNS = '01JB000000000000000WST0002';
const SECRET = 'synthetic-mock-secret-0001';

const github = {
  name: 'Demo repository',
  settings: { kind: 'github', repos: ['example-org/demo-repo'] },
  credential: 'gh_cli',
};

const jira = {
  name: 'Demo Jira',
  settings: {
    kind: 'jira',
    deployment: 'cloud',
    site: 'https://jira.example.com',
    projects: ['DEMO'],
    email: 'sam@example.com',
  },
  credential: 'stored',
};

test('integrations are device-only and checked', async () => {
  await withServer(async (server) => {
    assert.equal((await call(server, 'GET', '/v1/integrations', { token: AGENT })).status, 403);
    assert.equal((await call(server, 'POST', '/v1/integrations', { token: AGENT, json: github })).status, 403);
    for (const bad of [
      { ...github, name: ' ' },
      { ...github, credential: 'password' },
      { ...github, settings: { kind: 'github', repos: [] } },
      { ...github, settings: { kind: 'github', repos: ['not a repo'] } },
      { ...github, settings: { kind: 'github', repos: ['example-org/demo-repo'], api_base: 'http://ghe.example.com' } },
      { ...jira, credential: 'gh_cli' },
      { ...jira, settings: { ...jira.settings, email: undefined } },
      { ...jira, settings: { ...jira.settings, projects: ['DEMO" OR 1=1'] } },
      { ...github, interval_minutes: 1 },
    ]) {
      assert.equal((await call(server, 'POST', '/v1/integrations', { token: DEVICE, json: bad })).status, 400, JSON.stringify(bad));
    }
    const added = await call<Integration>(server, 'POST', '/v1/integrations', { token: DEVICE, json: github });
    assert.equal(added.status, 201);
    assert.deepEqual(added.body.credential, { source: 'gh_cli', stored: false });
    assert.equal(added.body.interval_minutes, 15);
    assert.equal((await call(server, 'POST', '/v1/integrations', { token: DEVICE, json: github })).status, 409);
    assert.equal((await call(server, 'GET', '/v1/integrations/01J00000000000000000000000', { token: DEVICE })).status, 404);
    // The sync's own member was added.
    assert.ok(server.hub.members.some((m) => m.handle === '@sync' && m.owner === ID.sam));
  });
});

test('a linked milestone syncs its open issues into tasks', async () => {
  await withServer(async (server) => {
    const added = await call<Integration>(server, 'POST', '/v1/integrations', { token: DEVICE, json: github });
    const linked = await call<Workstream>(server, 'PATCH', `/v1/workstreams/${SEED_RUNS}`, {
      token: DEVICE,
      json: { external: [{ system: 'github', key: 'example-org/demo-repo#milestone:1' }] },
    });
    assert.equal(linked.status, 200);
    assert.equal(linked.body.external.length, 1);
    assert.ok(server.hub.eventsAfter(0).some((e) => e.body.type === 'workstream_linked'));
    const synced = await call<Integration>(server, 'POST', `/v1/integrations/${added.body.id}/sync`, { token: DEVICE });
    assert.equal(synced.status, 202);
    assert.deepEqual(synced.body.status.problems, []);
    assert.ok(synced.body.status.last_success_at);
    assert.equal(synced.body.links[0]?.title, 'v1 launch');
    const tasks = await call<Task[]>(server, 'GET', `/v1/tasks?workstream=${SEED_RUNS}`, { token: DEVICE });
    const mirrored = tasks.body.filter((t) => t.source?.key.startsWith('example-org/') === true);
    assert.deepEqual(mirrored.map((t) => t.source?.key), ['example-org/demo-repo#1']);
    assert.equal(mirrored[0]?.title, 'Fix flaky login test');
    assert.equal(mirrored[0]?.status, 'todo');
    // Synced again: nothing new.
    const before = server.hub.rev;
    await call(server, 'POST', `/v1/integrations/${added.body.id}/sync`, { token: DEVICE });
    assert.equal(server.hub.rev, before);
    // Bad links are refused.
    for (const external of [
      [{ system: 'github', key: 'example-org/demo-repo', url: 'http://github.com/example-org/demo-repo' }],
      [{ system: 'jira', key: '' }],
      [{ system: 'jira', key: 'DEMO' }, { system: 'jira', key: 'DEMO' }],
    ]) {
      assert.equal((await call(server, 'PATCH', `/v1/workstreams/${SEED_RUNS}`, { token: DEVICE, json: { external } })).status, 400);
    }
    // A test reads once and warns about the credential's write rights.
    const check = await call<IntegrationCheck>(server, 'POST', `/v1/integrations/${added.body.id}/test`, { token: DEVICE });
    assert.equal(check.body.ok, true);
    assert.ok(check.body.warnings.length > 0);
  });
});

test('a stored credential is kept and never returned', async () => {
  await withServer(async (server) => {
    const added = await call<Integration>(server, 'POST', '/v1/integrations', { token: DEVICE, json: jira });
    assert.equal(added.status, 201);
    assert.equal(added.body.status.problems.length, 1);
    const path = `/v1/integrations/${added.body.id}/credential`;
    assert.equal((await call(server, 'PUT', path, { token: AGENT, json: { secret: SECRET } })).status, 403);
    assert.equal((await call(server, 'PUT', path, { token: DEVICE, json: { secret: 'two words' } })).status, 400);
    assert.equal((await call(server, 'PUT', path, { token: DEVICE, json: { secret: SECRET } })).status, 204);
    const one = await call<Integration>(server, 'GET', `/v1/integrations/${added.body.id}`, { token: DEVICE });
    assert.deepEqual(one.body.credential, { source: 'stored', stored: true });
    assert.deepEqual(one.body.status.problems, []);
    const check = await call<IntegrationCheck>(server, 'POST', `/v1/integrations/${added.body.id}/test`, { token: DEVICE });
    assert.equal(check.body.ok, true);
    const all = await call(server, 'GET', '/v1/integrations', { token: DEVICE });
    assert.ok(!JSON.stringify([one.body, check.body, all.body]).includes(SECRET));
    assert.ok(!JSON.stringify(server.hub.eventsAfter(0)).includes(SECRET));
    // A gh_cli connection keeps no secret.
    const gh = await call<Integration>(server, 'POST', '/v1/integrations', { token: DEVICE, json: github });
    assert.equal((await call(server, 'PUT', `/v1/integrations/${gh.body.id}/credential`, { token: DEVICE, json: { secret: SECRET } })).status, 409);
    assert.equal((await call(server, 'DELETE', `/v1/integrations/${added.body.id}`, { token: DEVICE })).status, 204);
    assert.equal((await call(server, 'GET', `/v1/integrations/${added.body.id}`, { token: DEVICE })).status, 404);
  });
});
