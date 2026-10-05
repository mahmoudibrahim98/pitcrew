// Run serially after the shared suite: inclusion changes every view of this disposable hub.
import assert from 'node:assert/strict';
import { test } from 'node:test';
const base = process.env.PITCREW_CONFORMANCE_URL;
const person = process.env.PITCREW_CONFORMANCE_PERSON;
const agent = process.env.PITCREW_CONFORMANCE_AGENT;
assert.ok(base && person && agent);
assert.ok(['127.0.0.1','localhost','[::1]'].includes(new URL(base).hostname));
async function call(method, path, body, token = person) {
  const response = await fetch(base + path, { method, headers: { Authorization: `Bearer ${token}`, 'Content-Type':'application/json' },
    ...(body === undefined ? {} : { body: JSON.stringify(body) }) });
  return { status: response.status, body: await response.json() };
}
async function ok(method, path, body) {
  const response = await call(method,path,body);
  assert.equal(response.status,200,JSON.stringify(response.body));
  return response.body;
}
// Counts leave sub-agents (sessions with a `parent`) out, and count them apart.
async function agreement(filter) {
  const dry = await ok('POST','/v1/import/dry-run',filter);
  const commit = await ok('PUT','/v1/import',filter);
  assert.ok(Number.isSafeInteger(dry.subagents) && dry.subagents >= 0, JSON.stringify(dry));
  assert.equal(dry.count,commit.imported);
  assert.equal(dry.subagents,commit.subagents);
  const listed = await ok('GET','/v1/sessions');
  assert.equal(listed.filter((s) => s.parent === undefined).length,commit.imported);
  assert.equal(listed.filter((s) => s.parent !== undefined).length,commit.subagents);
  // A sub-agent is listed exactly when its parent is.
  const ids = new Set(listed.map((s) => s.id));
  assert.ok(listed.every((s) => s.parent === undefined || ids.has(s.parent)));
  assert.equal((await ok('GET','/v1/import')).filter.mode,filter.mode);
  return commit.imported;
}
test('device-only reversible import: modes, dimensions, visibility and full restoration', async () => {
  for (const [method,path] of [['GET','/v1/import'],['PUT','/v1/import'],['POST','/v1/import/dry-run']]) {
    assert.equal((await call(method,path,method === 'GET' ? undefined : {mode:'all'},agent)).status,403);
  }
  const all = {mode:'all'};
  const total = await agreement(all);
  assert.ok(total > 0);
  const sessions = await ok('GET','/v1/sessions');
  const chosen = sessions.find((s) => s.parent === undefined);
  const blocks = await ok('GET','/v1/recaps/blocks');
  const filter = {mode:'filtered',engines:[chosen.engine],folders:[chosen.cwd]};
  const expected = sessions.filter((s) => s.parent === undefined && s.engine === chosen.engine &&
    (s.cwd === chosen.cwd || s.cwd.startsWith(chosen.cwd.replace(/\/$/,'') + '/')));
  assert.equal(await agreement(filter),expected.length);
  assert.equal(await agreement({mode:'filtered',since:'9999-12-31'}),0);
  assert.equal((await ok('GET','/v1/events?session='+chosen.id)).events.length,0);
  assert.equal((await call('GET','/v1/sessions/'+chosen.id)).status,404);
  assert.equal((await call('GET','/v1/sessions/'+chosen.id+'/transcript')).status,404);
  assert.equal((await ok('GET','/v1/recaps/blocks?session='+chosen.id)).blocks.length,0);
  const projects = await ok('GET','/v1/projects');
  for (const project of projects) {
    const days = await ok('GET','/v1/recaps/days?project='+project.id);
    const visibleBlocks = new Set((await ok('GET','/v1/recaps/blocks?project='+project.id)).blocks.map((b) => b.block.id));
    assert.ok(days.days.every((d) => d.blocks.every((id) => visibleBlocks.has(id))));
  }
  assert.equal(await agreement({mode:'none'}),0);
  assert.equal(await agreement(all),total);
  assert.equal((await call('GET','/v1/sessions/'+chosen.id)).status,200);
  assert.deepEqual(await ok('GET','/v1/recaps/blocks'),blocks);
  for (const since of ['2026-02-29','2026-09-31','invalid']) {
    assert.equal((await call('PUT','/v1/import',{mode:'filtered',since})).status,400);
  }
  assert.equal((await call('PUT','/v1/import',{mode:'filtered',folders:['']})).status,400);
  assert.equal((await call('PUT','/v1/import',{mode:'unknown'})).status,400);
  assert.equal((await ok('GET','/v1/sessions')).filter((s) => s.parent === undefined).length,total);
});
