import assert from 'node:assert/strict';
import { test } from 'node:test';

const base = process.env.PITCREW_CONFORMANCE_URL;
const person = process.env.PITCREW_CONFORMANCE_PERSON;
const agent = process.env.PITCREW_CONFORMANCE_AGENT;
const root = process.env.PITCREW_FILES_ROOT;
assert.ok(base && person && agent && root);
async function request(path, status = 200, body, token = person, method = body === undefined ? 'GET' : 'PUT') {
  const response = await fetch(new URL(path, base), { method, headers: { Authorization: `Bearer ${token}`, 'Content-Type': 'application/json' }, body: body === undefined ? undefined : JSON.stringify(body), signal: AbortSignal.timeout(30000) });
  const result = await response.json();
  assert.equal(response.status, status, JSON.stringify(result));
  return { result, response };
}
test('files: local tree, revisions, binary, bounds, device-only and remote refusal', { timeout: 120000 }, async t => {
  const { result: projects } = await request('/v1/projects');
  const { result: machines } = await request('/v1/machines');
  const local = machines.find(m => m.kind === 'local');
  const remote = machines.find(m => m.kind !== 'local');
  const { result: stream } = await request('/v1/workstreams', 201, { project: projects[0].id, name: 'Synthetic file conformance', locations: [{ machine: local.id, path: root }, { machine: remote.id, path: root }] }, person, 'POST');
  const route = `/v1/workstreams/${stream.id}/files`;
  const url = (path, content = true, loc = 0) => `${route}${content ? '/content' : ''}?${new URLSearchParams({loc: String(loc), path})}`;
  const { result: listing, response } = await request(url('', false));
  assert.equal(response.headers.get('cache-control'), 'no-store');
  assert.equal(response.headers.get('x-content-type-options'), 'nosniff');
  assert.equal(listing.truncated, false);
  assert.deepEqual(listing.entries.map(e => e.name), [...listing.entries.map(e => e.name)].sort());
  assert.ok(listing.entries.some(e => e.name === 'src' && e.kind === 'folder'));
  assert.ok(listing.entries.some(e => e.name === '.git' && e.kind === 'file'));
  assert.equal(listing.entries.find(e => e.name === 'debug.log').ignored, true);
  assert.equal(listing.entries.find(e => e.name === 'src').ignored, false);
  assert.equal((await request(url('debug.log'))).result.content, 'synthetic ignored file');
  const { result: initial } = await request(url('src/hello.txt'));
  assert.equal(initial.content, 'hello\n'); assert.equal(initial.encoding, 'utf8'); assert.equal(initial.size, 6);
  assert.match(initial.revision, /^[0-9a-f]{64}$/);
  const body = { revision: initial.revision, encoding: 'utf8', content: 'updated' };
  const { result: updated } = await request(url('src/hello.txt'), 200, body);
  assert.notEqual(updated.revision, initial.revision);
  const { result: conflict } = await request(url('src/hello.txt'), 409, body);
  assert.equal(conflict.current_revision, updated.revision);
  const binary = { revision: null, encoding: 'base64', content: '/wCA' };
  await request(url('new.bin'), 200, binary);
  assert.equal((await request(url('new.bin'))).result.content, binary.content);
  await request(url('new.bin'), 409, binary);
  await request(url('new.bin'), 400, { encoding: 'utf8', content: 'missing revision' });
  await request(url('new.bin'), 400, { revision: null, encoding: 'base64', content: 'bad=' });
  for (const encoding of [['utf8'], ['base64'], {}, null, 1, true]) {
    await request(url('malformed.txt'), 400, { revision: null, encoding, content: 'YWJj' });
    await request(url('malformed.txt'), 404);
  }
  for (const path of ['..', '../outside/secret', '/absolute', 'a\\b', 'src//hello.txt', 'src/./hello.txt', 'src/hello.txt/']) await request(url(path), 400);
  for (const path of ['.git', '.git/config', 'src/.git/config']) await request(url(path), 403, { revision: null, encoding: 'utf8', content: 'refused' });
  await request(url('', false), 403, undefined, agent);
  await request(url('src/hello.txt'), 403, undefined, agent);
  await request(url('new.bin'), 403, binary, agent);
  await request(url('missing'), 404);
  await request(url('', false, 99), 404);
  await request(url('', false, 1), 501);
  const { result: large } = await request(url('large.bin'), 413);
  assert.equal(large.size, 8 * 1024 * 1024 + 1);
  await request(url('huge'), 413, { revision: null, encoding: 'utf8', content: 'x'.repeat(8 * 1024 * 1024 + 1) });
  await request(url('huge'), 413, { revision: null, encoding: 'utf8', content: 'x'.repeat(12 * 1024 * 1024) });
  if (process.env.PITCREW_FILES_LINK) {
    assert.ok(listing.entries.some(e => e.name === 'outside' && e.kind === 'link'));
    await request(url('outside/secret'), 403);
    await request(url('outside/secret'), 403, binary);
  } else t.diagnostic('Link creation unavailable; runner link/junction tests cover supported platforms.');
});
