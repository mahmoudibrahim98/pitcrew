// Session import with sub-agents (api-v1.md, "Session import"): counts leave sub-agents out and
// count them apart, and a sub-agent is included exactly when its parent is.

import assert from 'node:assert/strict';
import { describe, it } from 'node:test';
import type { Session } from '../src/types.ts';
import { DEVICE, ID, call, withServer } from './helpers.ts';

interface DryRun {
  count: number;
  subagents: number;
}

describe('import counts', () => {
  it('nest sub-agents under their parents', () =>
    withServer(async (server) => {
      const all = await call<DryRun>(server, 'POST', '/v1/import/dry-run', { token: DEVICE, json: { mode: 'all' } });
      assert.equal(all.status, 200);
      const before = all.body;

      // A sub-agent of the first demo session, in another folder (a parent's folder decides).
      const parent = server.hub.sessions.find((s) => s.id === ID.ses1);
      assert.ok(parent);
      const sub: Session = {
        ...parent,
        id: '01JB000000000000000SES0099',
        native_id: 'agent-synthetic',
        cwd: `${parent.cwd}/elsewhere`,
        parent: parent.id,
        terminal: undefined,
      };
      server.hub.sessions.push(sub);

      const after = await call<DryRun>(server, 'POST', '/v1/import/dry-run', { token: DEVICE, json: { mode: 'all' } });
      assert.deepEqual(after.body, { count: before.count, subagents: before.subagents + 1 });

      const parentOnly = { mode: 'filtered', folders: [parent.cwd], engines: [parent.engine] };
      const committed = await call<{ imported: number; subagents: number }>(server, 'PUT', '/v1/import', {
        token: DEVICE,
        json: parentOnly,
      });
      assert.equal(committed.status, 200);
      assert.ok(committed.body.subagents >= 1);
      assert.equal((await call(server, 'GET', `/v1/sessions/${sub.id}`, { token: DEVICE })).status, 200);

      // Leaving the parent out leaves its sub-agent out, though the sub-agent's folder matches.
      const other = { mode: 'filtered', folders: [sub.cwd] };
      const none = await call<{ imported: number; subagents: number }>(server, 'PUT', '/v1/import', {
        token: DEVICE,
        json: other,
      });
      assert.deepEqual(none.body, { imported: 0, subagents: 0 });
      assert.equal((await call(server, 'GET', `/v1/sessions/${sub.id}`, { token: DEVICE })).status, 404);
    }));

  it('count a child whose chain of parents ends nowhere the hub knows as a session', () =>
    withServer(async (server) => {
      const dry = async () =>
        (await call<DryRun>(server, 'POST', '/v1/import/dry-run', { token: DEVICE, json: { mode: 'all' } })).body;
      const before = await dry();
      const parent = server.hub.sessions.find((s) => s.id === ID.ses1);
      assert.ok(parent);
      const stray = (id: string, named: string): Session => ({
        ...parent,
        id,
        native_id: `stray-${id}`,
        parent: named,
        terminal: undefined,
      });
      // One naming a parent the hub never saw, and two naming each other.
      server.hub.sessions.push(
        stray('01JB000000000000000SES0097', '01JB000000000000000SES0090'),
        stray('01JB000000000000000SES0095', '01JB000000000000000SES0096'),
        stray('01JB000000000000000SES0096', '01JB000000000000000SES0095'),
      );
      assert.deepEqual(await dry(), { count: before.count + 3, subagents: before.subagents });
    }));
});
