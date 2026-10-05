// GitHub and Jira integrations (api-v1.md, "Integrations"): device-only routes, links on
// workstreams, a sync over the recorded fixtures, and credentials that never come back.

import assert from 'node:assert/strict';
import { cpSync, mkdtempSync, readFileSync, rmSync, writeFileSync } from 'node:fs';
import { tmpdir } from 'node:os';
import { join } from 'node:path';
import { test } from 'node:test';
import { AGENT, DEVICE, ID, call, withServer } from './helpers.ts';
import type { RunningServer } from '../src/server.ts';
import type { Ask, Integration, IntegrationCheck, Project, Task, Workstream } from '../src/types.ts';

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

const SECOND = 'dev-second-device-token';
const LEE = '01JB000000000000000MEM0007';
const ISSUES = 'https://api.github.com/repos/example-org/demo-repo/issues?state=all&sort=updated&direction=asc&per_page=100';
const MILESTONES = 'https://api.github.com/repos/example-org/demo-repo/milestones?state=all&sort=due_on&direction=asc&per_page=100';
const FIXTURES = new URL('../fixtures/', import.meta.url);

/** Runs `run` against a server reading a copy of the fixtures, which `run` may change. */
async function withUpstream(run: (server: RunningServer, dir: string) => Promise<void>): Promise<void> {
  const dir = mkdtempSync(join(tmpdir(), 'pitcrew-mock-upstream-'));
  try {
    cpSync(FIXTURES, dir, { recursive: true });
    await withServer((server) => run(server, dir), { integrationFixtures: dir });
  } finally {
    rmSync(dir, { recursive: true, force: true });
  }
}

/** The recorded answer for `url`, as JSON. */
function recorded(url: string): Record<string, unknown>[] {
  const text = readRecorded();
  const block = text.split('### pitcrew-github-fixture ###').find((b) => b.trim().startsWith(`GET ${url} `));
  assert.ok(block, url);
  return JSON.parse(block.trim().split('\n\n').slice(2).join('\n\n')) as Record<string, unknown>[];
}
function readRecorded(): string {
  return readFileSync(new URL('github.fixture', FIXTURES), 'utf8');
}

/** From now on, "upstream" answers `body` for `url` (a fixture file that sorts first). */
function upstreamSays(dir: string, url: string, body: unknown): void {
  const name = url.includes('/milestones') ? '0-milestones' : '0-issues';
  writeFileSync(join(dir, `${name}.fixture`), `GET ${url} HTTP/1.1\nAccept: application/vnd.github+json\n\nHTTP/1.1 200\n\n${JSON.stringify(body)}\n`);
}

/** The recorded issues with `change` made to issue `number`. */
function issuesWith(issues: Record<string, unknown>[], number: number, change: (issue: Record<string, unknown>) => void): Record<string, unknown>[] {
  const copy = structuredClone(issues);
  const issue = copy.find((i) => i['number'] === number);
  assert.ok(issue);
  change(issue);
  issue['updated_at'] = '2026-01-03T09:00:00Z';
  return copy;
}

const MILESTONE_ONE = { number: 1, title: 'v1 launch', state: 'open', html_url: 'https://github.com/example-org/demo-repo/milestone/1' };

async function workstream(server: RunningServer, project: string, name: string): Promise<Workstream> {
  const made = await call<Workstream>(server, 'POST', '/v1/workstreams', { token: DEVICE, json: { project, name } });
  assert.equal(made.status, 201);
  return made.body;
}

test('moves, tasks and shipping follow upstream changes only', async () => {
  await withUpstream(async (server, dir) => {
    const project = await call<Project>(server, 'POST', '/v1/projects', { token: DEVICE, json: { key: 'SYN', name: 'Synthetic' } });
    assert.equal(project.status, 201);
    // Three workstreams: one on milestone 1, one busy one on milestone 1 too, one on milestone 2
    // (closed before the first read).
    const launch = await workstream(server, project.body.id, 'Launch');
    const busy = await workstream(server, project.body.id, 'Busy');
    const cleanup = await workstream(server, project.body.id, 'Cleanup');
    const own = await call<Task>(server, 'POST', '/v1/tasks', { token: DEVICE, json: { project: project.body.id, workstream: busy.id, title: 'Started by hand' } });
    assert.equal(own.status, 201);
    assert.equal((await call(server, 'POST', `/v1/tasks/${own.body.id}/move`, { token: DEVICE, json: { to: 'in_progress' } })).status, 200);
    const link = (key: string) => ({ external: [{ system: 'github', key }] });
    for (const [w, key] of [[launch, 'example-org/demo-repo#milestone:1'], [busy, 'example-org/demo-repo#milestone:1'], [cleanup, 'example-org/demo-repo#milestone:2']] as const) {
      assert.equal((await call(server, 'PATCH', `/v1/workstreams/${w.id}`, { token: DEVICE, json: link(key) })).status, 200);
    }
    const added = await call<Integration>(server, 'POST', '/v1/integrations', { token: DEVICE, json: github });
    const syncNow = async () => {
      const synced = await call<Integration>(server, 'POST', `/v1/integrations/${added.body.id}/sync`, { token: DEVICE });
      assert.deepEqual(synced.body.status.problems, []);
    };
    const mirrored = (key: string) => server.hub.tasks.find((t) => t.source?.key === key);
    const status = (id: string) => server.hub.workstreams.find((w) => w.id === id)?.status;
    assert.equal(mirrored('example-org/demo-repo#1')?.workstream, launch.id);
    assert.equal(mirrored('example-org/demo-repo#4'), undefined);
    assert.notEqual(status(cleanup.id), 'shipped', 'first seen closed: nothing ships');

    // #4 (open) joins milestone 1: it becomes a task there. #2 (closed) joins too, and stays out.
    const issues = recorded(ISSUES);
    let next = issuesWith(issues, 4, (i) => (i['milestone'] = MILESTONE_ONE));
    next = issuesWith(next, 2, (i) => (i['milestone'] = MILESTONE_ONE));
    upstreamSays(dir, ISSUES, next);
    await syncNow();
    assert.equal(mirrored('example-org/demo-repo#4')?.workstream, launch.id);
    assert.equal(mirrored('example-org/demo-repo#4')?.title, 'Write the release notes');
    assert.equal(mirrored('example-org/demo-repo#2'), undefined);

    // Upstream closes #1 and milestone 1: #1 is done, Launch ships, Busy (a task in progress)
    // gets a conflict ask instead.
    upstreamSays(dir, ISSUES, issuesWith(next, 1, (i) => {
      i['state'] = 'closed';
      i['state_reason'] = 'completed';
    }));
    upstreamSays(dir, MILESTONES, recorded(MILESTONES).map((m) => (m['number'] === 1 ? { ...m, state: 'closed' } : m)));
    await syncNow();
    const one = mirrored('example-org/demo-repo#1');
    assert.equal(one?.status, 'done');
    assert.equal(status(launch.id), 'shipped');
    assert.notEqual(status(busy.id), 'shipped');
    const asked = server.hub.asks.filter((a: Ask) => a.title === 'GitHub: example-org/demo-repo#milestone:1 needs a decision');
    assert.equal(asked.length, 1);
    assert.equal(asked[0]?.task, undefined);
    assert.equal(asked[0]?.to, ID.sam);

    // A person reopens #1's task: with nothing new upstream, the next sync leaves it, and asks
    // nothing again.
    assert.equal((await call(server, 'POST', `/v1/tasks/${one?.id ?? ''}/move`, { token: DEVICE, json: { to: 'todo' } })).status, 200);
    const before = server.hub.rev;
    await syncNow();
    assert.equal(mirrored('example-org/demo-repo#1')?.status, 'todo');
    assert.equal(server.hub.rev, before);
  });
});

test('each integration acts through its owner’s member, and a repository or project is one integration on any host', async () => {
  await withServer(async (server) => {
    server.hub.members.push({ id: LEE, kind: 'human', handle: '@lee', name: 'Lee' });
    const sams = await call<Integration>(server, 'POST', '/v1/integrations', { token: DEVICE, json: github });
    const lees = await call<Integration>(server, 'POST', '/v1/integrations', { token: SECOND, json: jira });
    assert.equal(sams.status, 201);
    assert.equal(lees.status, 201);
    const agentOf = (owner: string) => server.hub.members.find((m) => m.kind === 'agent' && m.owner === owner && ['@sync', '@tracker-sync'].includes(m.handle));
    assert.ok(agentOf(ID.sam) && agentOf(LEE) && agentOf(ID.sam)?.id !== agentOf(LEE)?.id);
    // Sam's sync still acts as Sam's member after Lee's integration was added.
    const linked = await call(server, 'PATCH', `/v1/workstreams/${SEED_RUNS}`, {
      token: DEVICE,
      json: { external: [{ system: 'github', key: 'example-org/demo-repo#milestone:1' }] },
    });
    assert.equal(linked.status, 200);
    await call(server, 'POST', `/v1/integrations/${sams.body.id}/sync`, { token: DEVICE });
    const created = server.hub.eventsAfter(0).find((e) => e.body.type === 'task_created' && e.body.data.task.source?.key === 'example-org/demo-repo#1');
    assert.equal(created?.author, agentOf(ID.sam)?.id);

    for (const [token, body] of [
      [DEVICE, { ...github, settings: { kind: 'github', repos: ['Example-Org/Demo-Repo'], api_base: 'https://ghe.example.com/api/v3' } }],
      [DEVICE, { ...jira, settings: { ...jira.settings, site: 'https://jira-b.example.com' } }],
    ] as const) {
      assert.equal((await call(server, 'POST', '/v1/integrations', { token, json: body })).status, 409, JSON.stringify(body));
    }
    // github.com's own API root is the default.
    const explicit = await call<Integration>(server, 'POST', '/v1/integrations', {
      token: DEVICE,
      json: { ...github, settings: { kind: 'github', repos: ['example-org/other-repo'], api_base: 'https://api.github.com/' } },
    });
    assert.equal(explicit.status, 201);
    assert.deepEqual(explicit.body.settings, { kind: 'github', repos: ['example-org/other-repo'] });
  });
});
