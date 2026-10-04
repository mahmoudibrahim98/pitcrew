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
  assert.match(member.handle, /^@agent-[0-9a-z]{26}$/);
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
    assert.notEqual(agents[0]?.handle, '@office');
    const task = await call<{ id: string }>(server, 'POST', '/v1/tasks', { token: DEVICE, json: { project: project.body.id, title: `Run ${engine}` } });
    assert.equal((await call(server, 'POST', `/v1/tasks/${task.body.id}/dispatch`, { token: DEVICE, json: { agent: agents[0]?.id } })).status, 202);
  }
  const before = (await call<{ rev: number }>(server, 'GET', '/v1/workspace', { token: DEVICE })).body.rev;
  await scan();
  assert.equal((await call<{ rev: number }>(server, 'GET', '/v1/workspace', { token: DEVICE })).body.rev, before);
}, { fresh: true }));
