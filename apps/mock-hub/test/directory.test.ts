import assert from 'node:assert/strict';
import { it } from 'node:test';
import { DEVICE, call, withServer } from './helpers.ts';
import type { Persona, Team, Member } from '../src/types.ts';

it('directory edits accept prefixed ids and refuse malformed fields without changing state', () => withServer(async (server) => {
  const recipe = await call<Persona>(server, 'POST', '/v1/personas', { token: DEVICE,
    json: { name: '\uFEFFSynthetic writer\uFEFF', engine: 'codex' } });
  assert.equal(recipe.status, 201); assert.equal(recipe.body.name, 'Synthetic writer');
  const renamed = await call<Persona>(server, 'PUT', `/v1/personas/per_${recipe.body.id}`, { token: DEVICE,
    json: { name: 'Renamed writer', engine: 'claude' } });
  assert.equal(renamed.status, 200); assert.equal(renamed.body.id, recipe.body.id);
  for (const json of [{ name: 'Bad\u0085', engine: 'codex' }, { name: 'Valid', engine: 'codex', permission_mode: null }]) {
    assert.equal((await call(server, 'POST', '/v1/personas', { token: DEVICE, json })).status, 400);
  }
  const members = await call<Member[]>(server, 'GET', '/v1/members', { token: DEVICE });
  const member = members.body.find((m) => m.persona === recipe.body.id);
  assert.ok(member); assert.equal(member.name, 'Renamed writer');
  assert.equal(member.handle, '@codex');
  const team = await call<Team>(server, 'POST', '/v1/teams', { token: DEVICE,
    json: { name: 'Synthetic crew', lead: member.id, members: [] } });
  assert.equal(team.status, 201);
  const edit = await call<Team>(server, 'PUT', `/v1/teams/team_${team.body.id}`, { token: DEVICE,
    json: { name: 'Renamed crew', lead: member.id, members: [] } });
  assert.equal(edit.status, 200); assert.equal(edit.body.name, 'Renamed crew');
  const before = (await call<Persona[]>(server, 'GET', '/v1/personas', { token: DEVICE })).body;
  const refused = await call(server, 'PUT', `/v1/personas/${recipe.body.id}`, { token: DEVICE,
    json: { name: 'Bad', engine: 'codex', model: ' ' } });
  assert.equal(refused.status, 400);
  assert.deepEqual((await call<Persona[]>(server, 'GET', '/v1/personas', { token: DEVICE })).body, before);
}));

it('a fresh setup scan provisions owned agents for every detected engine and can dispatch them', () => withServer(async (server) => {
  const setup = await call<{ me: Member; machine: { id: string } }>(server, 'POST', '/v1/setup', { token: DEVICE,
    json: { workspace_name: 'Synthetic lab', person: { name: 'Sam Rivera', handle: '@sam' }, machine_name: 'Test machine' } });
  assert.equal(setup.status, 200);
  const scan = async (): Promise<void> => {
    const response = await fetch(`${server.url}/v1/machines/${setup.body.machine.id}/scan`, { method: 'POST', headers: { authorization: `Bearer ${DEVICE}` } });
    assert.equal(response.status, 200);
    assert.match(await response.text(), /"type":"done"/);
  };
  await scan();
  const members = (await call<Member[]>(server, 'GET', '/v1/members', { token: DEVICE })).body;
  const personas = (await call<Persona[]>(server, 'GET', '/v1/personas', { token: DEVICE })).body;
  const project = await call<{ id: string }>(server, 'POST', '/v1/projects', { token: DEVICE, json: { name: 'Synthetic', key: 'SYN' } });
  for (const engine of ['claude', 'codex', 'opencode']) {
    const agents = members.filter((m) => m.kind === 'agent' && m.owner === setup.body.me.id && personas.some((p) => p.id === m.persona && p.engine === engine));
    assert.equal(agents.length, 1, engine);
    assert.equal(agents[0]?.handle, `@${engine}`);
    const task = await call<{ id: string }>(server, 'POST', '/v1/tasks', { token: DEVICE, json: { project: project.body.id, title: `Run ${engine}` } });
    assert.equal((await call(server, 'POST', `/v1/tasks/${task.body.id}/dispatch`, { token: DEVICE, json: { agent: agents[0]?.id } })).status, 202);
  }
  const before = (await call<{ rev: number }>(server, 'GET', '/v1/workspace', { token: DEVICE })).body.rev;
  await scan();
  assert.equal((await call<{ rev: number }>(server, 'GET', '/v1/workspace', { token: DEVICE })).body.rev, before);
}, { fresh: true }));

it('persona edits require every linked member to belong to the caller and reject unsafe settings atomically', () => withServer(async (server) => {
  const recipe = await call<Persona>(server, 'POST', '/v1/personas', { token: DEVICE, json: { name: 'Synthetic', engine: 'opencode' } });
  const second = await call<Persona>(server, 'POST', '/v1/personas', { token: DEVICE, json: { name: 'Synthetic collision', engine: 'opencode' } });
  assert.equal(server.hub.members.find((m) => m.persona === recipe.body.id)?.handle, '@opencode');
  assert.equal(server.hub.members.find((m) => m.persona === second.body.id)?.handle, '@opencode-2');
  const before = structuredClone(server.hub.personas);
  for (const change of [{ model: ' --dangerously-skip-permissions ' }, { permission_mode: 'bypass_permissions' }]) {
    for (const [method, path] of [['POST', '/v1/personas'], ['PUT', `/v1/personas/${recipe.body.id}`]]) {
      assert.equal((await call(server, method!, path!, { token: DEVICE, json: { name: 'Unsafe', engine: 'claude', ...change } })).status, 400);
    }
  }
  assert.equal((await call(server, 'PUT', `/v1/personas/${recipe.body.id}`, { token: 'dev-second-device-token', json: { name: 'Stolen', engine: 'claude' } })).status, 403);
  const own = server.hub.members.find((m) => m.persona === recipe.body.id)!;
  server.hub.members.push({ ...own, id: '01J00000000000000000000001', handle: '@foreign', owner: '01JB000000000000000MEM0007' });
  const rev = server.hub.rev;
  assert.equal((await call(server, 'PUT', `/v1/personas/${recipe.body.id}`, { token: DEVICE, json: { name: 'Stolen', engine: 'claude' } })).status, 403);
  assert.deepEqual(server.hub.personas, before); assert.equal(server.hub.rev, rev);
  for (let i = server.hub.members.length - 1; i >= 0; i--) if (server.hub.members[i]?.persona === recipe.body.id) server.hub.members.splice(i, 1);
  assert.equal((await call(server, 'PUT', `/v1/personas/${recipe.body.id}`, { token: DEVICE, json: { name: 'Unlinked', engine: 'claude' } })).status, 200);
}));

it('setup reports its platform, rejects incompatible or relative local roots, and refuses service-actor dispatch', () => withServer(async (server) => {
  const setup = await call<{ me: Member; machine: { id: string; info: { os: string; arch: string } } }>(server, 'POST', '/v1/setup', { token: DEVICE,
    json: { workspace_name: 'Synthetic', person: { name: 'Synthetic person', handle: '@synthetic' }, machine_name: 'Synthetic machine' } });
  assert.equal(setup.body.machine.info.os, process.platform === 'win32' ? 'windows' : process.platform === 'darwin' ? 'macos' : process.platform);
  assert.ok(setup.body.machine.info.arch);
  const rev = server.hub.rev;
  for (const path of ['relative', process.platform === 'win32' ? '/unix/path' : 'C:\\synthetic\\path']) {
    assert.equal((await call(server, 'POST', '/v1/projects', { token: DEVICE, json: { name: 'Invalid', key: 'BAD', root: { machine: setup.body.machine.id, path } } })).status, 400);
  }
  assert.equal(server.hub.rev, rev);
  const project = await call<{ id: string }>(server, 'POST', '/v1/projects', { token: DEVICE, json: { name: 'Synthetic', key: 'SYN', root: { machine: setup.body.machine.id, path: process.platform === 'win32' ? 'C:\\synthetic' : '/synthetic' } } });
  assert.equal(project.status, 201);
  const task = await call<{ id: string }>(server, 'POST', '/v1/tasks', { token: DEVICE, json: { project: project.body.id, title: 'Synthetic task' } });
  server.hub.members.push({ id: '01J00000000000000000000002', kind: 'agent', handle: '@synthetic-sync', name: 'Synthetic sync', owner: setup.body.me.id });
  const beforeDispatch = server.hub.rev;
  assert.equal((await call(server, 'POST', `/v1/tasks/${task.body.id}/dispatch`, { token: 'dev-second-device-token', json: { agent: '01J00000000000000000000002' } })).status, 403);
  assert.equal((await call(server, 'POST', `/v1/tasks/${task.body.id}/dispatch`, { token: DEVICE, json: { agent: '01J00000000000000000000002' } })).status, 400);
  assert.equal(server.hub.rev, beforeDispatch);
}, { fresh: true }));
