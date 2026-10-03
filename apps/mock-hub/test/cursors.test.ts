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

test('activity skips metadata before counting the limit and preserves revisions', async () => {
  await withServer(async (server) => {
    for (let rev = 15; rev < 75; rev++) {
      await call(server, 'PUT', '/v1/me/cursors/workspace', { token: DEVICE, json: { rev } });
    }
    for (const route of ['/v1/events', '/v1/activity']) {
      const { body } = await call<{ events: { body: { type: string } }[]; revisions: number[]; from_rev: number; to_rev: number; at_start: boolean }>(server, 'GET', `${route}?limit=10`, { token: DEVICE });
      assert.equal(body.events.length, 10);
      assert.ok(body.events.every((e) => e.body.type !== 'cursor_moved'));
      assert.deepEqual(body.revisions, [6,7,8,9,10,11,12,13,14,15]);
      assert.equal(body.from_rev, 6); assert.equal(body.to_rev, 15); assert.equal(body.at_start, false);
    }
  });
});
