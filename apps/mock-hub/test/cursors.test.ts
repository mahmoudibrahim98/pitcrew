import assert from 'node:assert/strict';
import { test } from 'node:test';
import { AGENT, DEVICE, ID, call, withServer } from './helpers.ts';

test('read cursors move forward per person and scope and reject agents', async () => {
  await withServer(async (server) => {
    for (const scope of ['workspace', `project:${ID.paper}`, `workstream:${ID.submission}`]) {
      const path = `/v1/me/cursors/${encodeURIComponent(scope)}`;
      for (const rev of [10, 4, 10, 0]) {
        assert.deepEqual((await call(server, 'PUT', path, { token: DEVICE, json: { rev } })).body, { scope, rev: 10 });
      }
      assert.deepEqual((await call(server, 'PUT', path, { token: 'dev-second-device-token', json: { rev: 3 } })).body, { scope, rev: 3 });
    }
    assert.equal((await call<unknown[]>(server, 'GET', '/v1/me/cursors', { token: DEVICE })).body.length, 3);
    assert.equal((await call(server, 'GET', '/v1/me/cursors', { token: AGENT })).status, 403);
    assert.equal((await call(server, 'PUT', '/v1/me/cursors/workspace', { token: AGENT, raw: '{' })).status, 403);
    for (const rev of [-1, 1.5, '10', 1_000_000]) {
      assert.equal((await call(server, 'PUT', '/v1/me/cursors/workspace', { token: DEVICE, json: { rev } })).status, 400);
    }
    assert.equal((await call(server, 'PUT', '/v1/me/cursors/task:bad', { token: DEVICE, json: { rev: 1 } })).status, 400);
  });
});
