import assert from 'node:assert/strict';
import { test } from 'node:test';
const base = process.env.PITCREW_CONFORMANCE_URL;
const person = process.env.PITCREW_CONFORMANCE_PERSON;
const agent = process.env.PITCREW_CONFORMANCE_AGENT;
assert.ok(base && person && agent);
async function request(method, path, status, body, token = person) {
  const response = await fetch(new URL(path, base), { method, headers: { Authorization: `Bearer ${token}`, 'Content-Type': 'application/json' }, body: body === undefined ? undefined : JSON.stringify(body), signal: AbortSignal.timeout(30000) });
  const value = await response.json();
  assert.equal(response.status, status, JSON.stringify(value));
  return value;
}
test('onboarding: exact preview confirmation, replay, device-only routes and safety persistence', { skip: process.env.PITCREW_CONFORMANCE_SYNTHETIC_HOOKS !== '1' }, async () => {
  const machines = await request('GET', '/v1/machines', 200);
  const own = machines.find(m => m.kind === 'local');
  const other = machines.find(m => m.kind !== 'local');
  const diffPath = `/v1/machines/${own.id}/hooks/diff`;
  const applyPath = `/v1/machines/${own.id}/hooks/install`;
  for (const [method, path, body] of [['POST', diffPath], ['POST', applyPath, { revision: 'invalid' }], ['GET', '/v1/safety'], ['PUT', '/v1/safety', {}]]) await request(method, path, 403, body, agent);
  await request('POST', `/v1/machines/${other.id}/hooks/diff`, 501);
  await request('POST', '/v1/machines/unknown/hooks/diff', 404);
  await request('POST', applyPath, 409, { revision: 'unknown' });
  await request('POST', applyPath, 400, { revision: 'unknown', files: [] });
  const diff = await request('POST', diffPath, 200);
  assert.ok(typeof diff.revision === 'string' && diff.revision.length > 0);
  assert.ok(diff.files.length > 0);
  await request('POST', applyPath, 409, { revision: diff.revision }, process.env.PITCREW_CONFORMANCE_SECOND_PERSON);
  for (const file of diff.files) { assert.equal(typeof file.path, 'string'); assert.ok(file.before === null || typeof file.before === 'string'); assert.equal(typeof file.after, 'string'); }
  assert.deepEqual(await request('POST', applyPath, 200, { revision: diff.revision }), { installed: true, skipped: [] });
  await request('POST', applyPath, 200, { revision: diff.revision });
  assert.deepEqual((await request('POST', diffPath, 200)).files, []);
  const original = await request('GET', '/v1/safety', 200);
  const settings = { permission_mode: 'plan', back_office_enabled: true, back_office_caps: { max_auto_accept_per_hour: 7 } };
  assert.deepEqual(await request('PUT', '/v1/safety', 200, settings), settings);
  assert.deepEqual(await request('GET', '/v1/safety', 200), settings);
  await request('PUT', '/v1/safety', 200, settings);
  for (const cap of [-1, 101, 1.5, '7', null]) await request('PUT', '/v1/safety', 400, { ...settings, back_office_caps: { max_auto_accept_per_hour: cap } });
  await request('PUT', '/v1/safety', 400, { ...settings, permission_mode: 'unexpected' });
  await request('PUT', '/v1/safety', 400, { ...settings, permission_mode: 'bypass_permissions' });
  assert.deepEqual(await request('GET', '/v1/safety', 200), settings);
  const { saved: _saved, ...restore } = original;
  await request('PUT', '/v1/safety', 200, restore);
});
