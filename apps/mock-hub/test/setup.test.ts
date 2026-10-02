// `POST /v1/setup` and `GET /v1/workspace`'s `setup_needed`, in demo mode and in fresh mode
// (`PITCREW_MOCK_FRESH=1`; api-v1.md, "The first run").

import assert from 'node:assert/strict';
import { describe, it } from 'node:test';
import type { RunningServer } from '../src/server.ts';
import type { ApiError, Machine, Member, StreamFrame, Workspace } from '../src/types.ts';
import { AGENT, DEVICE, ID, call, withServer } from './helpers.ts';
import { TestSocket } from './ws-client.ts';

type EventsFrame = Extract<StreamFrame, { type: 'events' }>;

interface WorkspaceReply {
  workspace: Workspace;
  rev: number;
  setup_needed?: boolean;
}

interface SetupDone {
  workspace: Workspace;
  me: Member;
  machine: Machine;
}

/** Asserts an error reply's status and code. */
function refused(res: { status: number; body: unknown }, status: number, what: string): void {
  const codes: Record<number, string> = { 400: 'invalid', 403: 'forbidden', 404: 'not_found', 409: 'conflict' };
  assert.equal(res.status, status, what);
  assert.equal((res.body as ApiError).code, codes[status], what);
}

/** The contract's own example (api-v1.md, "The first run"). */
const SETUP = {
  workspace_name: 'Demo Lab',
  person: { name: 'Sam Rivera', handle: '@sam' },
  machine_name: 'This laptop',
};

/** Runs `test` against a fresh (unset-up) workspace. */
function fresh(test: (server: RunningServer) => Promise<void>): Promise<void> {
  return withServer(test, { fresh: true });
}

describe('GET /v1/workspace: setup_needed', () => {
  it('is omitted once the workspace has a person, as the demo does', () =>
    withServer(async (server) => {
      const res = await call<WorkspaceReply>(server, 'GET', '/v1/workspace', { token: DEVICE });
      assert.equal(res.status, 200);
      assert.equal(res.body.setup_needed, undefined);
    }));

  it('is true for a fresh workspace, which starts at revision 0', () =>
    fresh(async (server) => {
      const res = await call<WorkspaceReply>(server, 'GET', '/v1/workspace', { token: DEVICE });
      assert.equal(res.status, 200);
      assert.equal(res.body.setup_needed, true);
      assert.equal(res.body.rev, 0);
    }));
});

describe('GET /v1/me before setup', () => {
  it('answers 404 for both the device and the agent token on a fresh workspace', () =>
    fresh(async (server) => {
      refused(await call<ApiError>(server, 'GET', '/v1/me', { token: DEVICE }), 404, 'device');
      refused(await call<ApiError>(server, 'GET', '/v1/me', { token: AGENT }), 404, 'agent');
    }));

  it('still authenticates the tokens: other routes answer normally, with empty lists', () =>
    fresh(async (server) => {
      const machines = await call<Machine[]>(server, 'GET', '/v1/machines', { token: DEVICE });
      assert.deepEqual(machines.body, []);
      const members = await call<Member[]>(server, 'GET', '/v1/members', { token: AGENT });
      assert.deepEqual(members.body, []);
    }));
});

describe('POST /v1/setup', () => {
  it('always answers 409 in demo mode, whatever the body holds', () =>
    withServer(async (server) => {
      refused(await call(server, 'POST', '/v1/setup', { token: DEVICE, json: SETUP }), 409, 'valid body');
      refused(
        await call(server, 'POST', '/v1/setup', { token: DEVICE, json: { nonsense: true } }),
        409,
        'malformed body',
      );
    }));

  it('needs a device token: an agent token gets 403, even on a fresh workspace', () =>
    fresh(async (server) => {
      refused(await call(server, 'POST', '/v1/setup', { token: AGENT, json: SETUP }), 403, 'agent token');
    }));

  it('validates every field exactly as the contract says, and changes nothing', () =>
    fresh(async (server) => {
      const long = (n: number): string => 'x'.repeat(n);
      const cases: unknown[] = [
        {},
        { ...SETUP, workspace_name: '' },
        { ...SETUP, workspace_name: long(81) },
        { ...SETUP, person: { name: '', handle: '@sam' } },
        { ...SETUP, person: { name: long(81), handle: '@sam' } },
        { ...SETUP, person: { name: 'Sam', handle: 'sam' } }, // missing "@"
        { ...SETUP, person: { name: 'Sam', handle: '@Sam' } }, // upper case
        { ...SETUP, person: { name: 'Sam', handle: '@sam!' } }, // bad character
        { ...SETUP, person: { name: 'Sam', handle: `@${long(33)}` } }, // too long
        { ...SETUP, machine_name: '' },
        { ...SETUP, machine_name: long(61) },
        { ...SETUP, workspace_name: 'Lab\u0007' },
        { ...SETUP, person: { name: 'Sam\u0007', handle: '@sam' } },
        { ...SETUP, machine_name: 'Laptop\u0007' },
        // Nothing but whitespace is empty once trimmed.
        { ...SETUP, workspace_name: ' \t\n ' },
        { ...SETUP, person: { name: ' 　', handle: '@sam' } },
        { ...SETUP, machine_name: '  ' },
        // Counted after trimming: still too long.
        { ...SETUP, workspace_name: ` ${long(81)} ` },
        // The handle is not trimmed.
        { ...SETUP, person: { name: 'Sam', handle: ' @sam' } },
        { ...SETUP, person: { name: 'Sam', handle: '@sam ' } },
        // U+0085 at an end is a control character, which trimming keeps: refused.
        { ...SETUP, workspace_name: 'Lab\u0085' },
      ];
      for (const json of cases) {
        refused(await call(server, 'POST', '/v1/setup', { token: DEVICE, json }), 400, JSON.stringify(json));
      }
      const workspace = await call<WorkspaceReply>(server, 'GET', '/v1/workspace', { token: DEVICE });
      assert.equal(workspace.body.setup_needed, true, 'nothing above should have set the workspace up');
      assert.equal(workspace.body.rev, 0, 'nothing above should have appended an event');
    }));

  it('sets the workspace up once, naming the device token\'s own member and this machine', () =>
    fresh(async (server) => {
      const res = await call<SetupDone>(server, 'POST', '/v1/setup', { token: DEVICE, json: SETUP });
      assert.equal(res.status, 200);
      assert.equal(res.body.workspace.name, 'Demo Lab');

      const { me, machine } = res.body;
      assert.equal(me.id, ID.sam, 'the person is the device token\'s own member id');
      assert.equal(me.kind, 'human');
      assert.equal(me.handle, '@sam');
      assert.equal(me.name, 'Sam Rivera');
      assert.equal(me.owner, undefined);

      assert.equal(machine.id.length, 26);
      assert.equal(machine.kind, 'local');
      assert.equal(machine.name, 'This laptop');
      assert.equal(machine.liveness, 'live');

      // GET /v1/me now means this person, and setup_needed is false.
      const gotMe = await call<Member>(server, 'GET', '/v1/me', { token: DEVICE });
      assert.equal(gotMe.status, 200);
      assert.deepEqual(gotMe.body, me);

      const workspace = await call<WorkspaceReply>(server, 'GET', '/v1/workspace', { token: DEVICE });
      assert.equal(workspace.body.setup_needed, undefined);
      assert.equal(workspace.body.rev, 2);

      const machines = await call<Machine[]>(server, 'GET', '/v1/machines', { token: DEVICE });
      assert.deepEqual(machines.body, [machine]);
    }));

  it('trims the three names, counts them after trimming, and stores them trimmed', () =>
    fresh(async (server) => {
      const padded = {
        workspace_name: `  ${'x'.repeat(80)}\t`,
        person: { name: '　Sam Rivera\n', handle: '@sam' },
        machine_name: ' This  laptop ',
      };
      const res = await call<SetupDone>(server, 'POST', '/v1/setup', { token: DEVICE, json: padded });
      assert.equal(res.status, 200);
      assert.equal(res.body.workspace.name, 'x'.repeat(80));
      assert.equal(res.body.me.name, 'Sam Rivera');
      assert.equal(res.body.machine.name, 'This  laptop', 'whitespace inside a name stays');

      const workspace = await call<WorkspaceReply>(server, 'GET', '/v1/workspace', { token: DEVICE });
      assert.equal(workspace.body.workspace.name, 'x'.repeat(80));
      const me = await call<Member>(server, 'GET', '/v1/me', { token: DEVICE });
      assert.equal(me.body.name, 'Sam Rivera');
      const machines = await call<Machine[]>(server, 'GET', '/v1/machines', { token: DEVICE });
      assert.equal(machines.body[0]?.name, 'This  laptop');
    }));

  it('answers 409 once set up, and never creates a second person', () =>
    fresh(async (server) => {
      const first = await call<SetupDone>(server, 'POST', '/v1/setup', { token: DEVICE, json: SETUP });
      assert.equal(first.status, 200);
      const second = await call(server, 'POST', '/v1/setup', {
        token: DEVICE,
        json: { ...SETUP, person: { name: 'Someone Else', handle: '@someone' } },
      });
      refused(second, 409, 'second setup');
      const members = await call<Member[]>(server, 'GET', '/v1/members', { token: DEVICE });
      assert.equal(members.body.length, 1);
      assert.equal(members.body[0]?.handle, '@sam');
    }));

  it('answers 409 on a handle clash, even before a person exists', () =>
    fresh(async (server) => {
      // As if an agent had been configured before anyone set the workspace up.
      server.hub.members.push({ id: '01JB000000000000000MEM0099', kind: 'agent', handle: '@helper', name: 'Helper' });
      const res = await call(server, 'POST', '/v1/setup', {
        token: DEVICE,
        json: { ...SETUP, person: { name: 'Sam Rivera', handle: '@helper' } },
      });
      refused(res, 409, 'handle taken');
      const workspace = await call<WorkspaceReply>(server, 'GET', '/v1/workspace', { token: DEVICE });
      assert.equal(workspace.body.setup_needed, true, 'the clash must not have set the workspace up');
    }));

  it('reserves @office for the back office, though no member holds it', () =>
    fresh(async (server) => {
      assert.equal(server.hub.members.length, 0);
      const office = { name: 'Sam Rivera', handle: '@office' };
      const res = await call<ApiError>(server, 'POST', '/v1/setup', {
        token: DEVICE,
        json: { ...SETUP, person: office },
      });
      refused(res, 409, 'reserved handle');
      assert.match(res.body.message, /reserved/);
      // A 400 still comes first.
      refused(
        await call(server, 'POST', '/v1/setup', {
          token: DEVICE,
          json: { ...SETUP, person: office, machine_name: 'x'.repeat(61) },
        }),
        400,
        'reserved handle and a machine name too long',
      );
      const workspace = await call<WorkspaceReply>(server, 'GET', '/v1/workspace', { token: DEVICE });
      assert.equal(workspace.body.setup_needed, true, 'the reserved handle must not have set the workspace up');
      assert.equal(workspace.body.rev, 0);
      // Any other handle still sets it up.
      const done = await call<SetupDone>(server, 'POST', '/v1/setup', { token: DEVICE, json: SETUP });
      assert.equal(done.status, 200);
    }));

  it('appends member_added then machine_added, which a connected stream sees live', () =>
    fresh(async (server) => {
      const socket = await TestSocket.connect(`${server.url}/v1/stream`, ['pitcrew.v1', `pitcrew.bearer.${DEVICE}`]);
      assert.deepEqual(await socket.nextJson(), { type: 'hello', rev: 0, log: server.logId });

      const res = await call<SetupDone>(server, 'POST', '/v1/setup', { token: DEVICE, json: SETUP });
      assert.equal(res.status, 200);

      const frame = await socket.nextJson<EventsFrame>();
      assert.equal(frame.type, 'events');
      assert.equal(frame.from_rev, 1);
      assert.equal(frame.to_rev, 2);
      assert.deepEqual(
        frame.events.map((e) => e.body.type),
        ['member_added', 'machine_added'],
      );
      for (const event of frame.events) {
        assert.equal(event.author, res.body.me.id, 'both events are authored by the person');
        assert.equal(event.on_behalf_of, undefined);
      }
      assert.deepEqual(frame.events[0]?.body, { type: 'member_added', data: { member: res.body.me } });
      assert.deepEqual(frame.events[1]?.body, { type: 'machine_added', data: { machine: res.body.machine } });
      socket.close();
    }));
});
