// Machine setup (api-v1.md, "Machine setup"): the check, the accounts and a sign-in terminal, on
// the hub's own machine only, for device tokens only.

import assert from 'node:assert/strict';
import { describe, it } from 'node:test';
import type { AgentAccount, MachineCheckRow, SignIn } from '../src/machine-setup.ts';
import type { RunningServer } from '../src/server.ts';
import type { ApiError } from '../src/types.ts';
import { AGENT, DEVICE, call, sleep, withServer } from './helpers.ts';
import { TestSocket, type Message } from './ws-client.ts';

const LAPTOP = '01JB000000000000000MCH0001';
const CLUSTER = '01JB000000000000000MCH0002';
const FAST = { delays: { signIn: 60 } };
const SLOW = { delays: { signIn: 60_000 } };

const signInPath = (engine: string, machine = LAPTOP): string => `/v1/machines/${machine}/agents/${engine}/sign-in`;

function terminal(server: RunningServer, id: string): Promise<TestSocket> {
  return TestSocket.connect(`${server.url}/v1/sessions/${id}/terminal?cols=80&rows=24`, [
    'pitcrew.v1',
    `pitcrew.bearer.${DEVICE}`,
  ]);
}

/** Reads until the socket closes; the screen's text and the text frames. */
async function readToClose(socket: TestSocket): Promise<{ screen: string; texts: string[]; code: number }> {
  let screen = '';
  const texts: string[] = [];
  for (;;) {
    const message: Message = await socket.next();
    if (message.type === 'binary') screen += message.data.toString('utf8');
    else if (message.type === 'text') texts.push(message.text);
    else if (message.type === 'close') return { screen, texts, code: message.code };
  }
}

describe('machine check', () => {
  it('answers fixed rows for the hub’s own machine, one row on request', () =>
    withServer(async (server) => {
      const all = await call<{ rows: MachineCheckRow[] }>(server, 'GET', `/v1/machines/${LAPTOP}/check`, { token: DEVICE });
      assert.equal(all.status, 200);
      assert.deepEqual(
        all.body.rows.map((r) => r.id),
        ['cli_claude', 'cli_codex', 'cli_opencode', 'tmux', 'git', 'gh', 'disk'],
      );
      for (const row of all.body.rows) {
        if (row.status === 'ok') assert.equal(row.fix, undefined, row.id);
        else assert.equal(row.fix, 'install_page', row.id);
      }
      const one = await call<{ rows: MachineCheckRow[] }>(server, 'GET', `/v1/machines/${LAPTOP}/check?row=gh`, { token: DEVICE });
      assert.deepEqual(one.body.rows.map((r) => r.id), ['gh']);
      const bad = await call<ApiError>(server, 'GET', `/v1/machines/${LAPTOP}/check?row=cli-claude`, { token: DEVICE });
      assert.equal(bad.status, 400);
      assert.equal(bad.body.code, 'invalid');
    }));

  it('refuses agents, other machines and unknown ones', () =>
    withServer(async (server) => {
      for (const path of [`/v1/machines/${LAPTOP}/check`, `/v1/machines/${LAPTOP}/agents`]) {
        assert.equal((await call(server, 'GET', path, { token: AGENT })).status, 403);
        assert.equal((await call(server, 'GET', path)).status, 401);
      }
      const other = await call<ApiError>(server, 'GET', `/v1/machines/${CLUSTER}/check`, { token: DEVICE });
      assert.equal(other.status, 409);
      assert.equal(other.body.code, 'conflict');
      assert.equal((await call(server, 'GET', `/v1/machines/${CLUSTER}/agents`, { token: DEVICE })).status, 409);
      const unknown = '01J00000000000000000000000';
      assert.equal((await call(server, 'GET', `/v1/machines/${unknown}/check`, { token: DEVICE })).status, 404);
      assert.equal((await call(server, 'POST', signInPath('claude', CLUSTER), { token: DEVICE })).status, 409);
      assert.equal((await call(server, 'POST', signInPath('claude'), { token: AGENT })).status, 403);
    }));
});

describe('sign-in', () => {
  it('runs a canned login whose end signs the CLI in', () =>
    withServer(async (server) => {
      const before = await call<AgentAccount[]>(server, 'GET', `/v1/machines/${LAPTOP}/agents`, { token: DEVICE });
      assert.deepEqual(before.body, [
        { engine: 'claude', installed: true, signed_in: false },
        { engine: 'codex', installed: true, signed_in: true, account: 'ChatGPT' },
        { engine: 'opencode', installed: false, detail: 'OpenCode (opencode) is not on PATH.' },
      ]);
      const started = await call<SignIn>(server, 'POST', signInPath('claude'), { token: DEVICE });
      assert.equal(started.status, 201);
      assert.deepEqual(started.body.command, ['claude', 'auth', 'login']);
      assert.equal(started.body.running, true);
      const again = await call<SignIn>(server, 'POST', signInPath('claude'), { token: DEVICE, json: {} });
      assert.equal(again.status, 200);
      assert.equal(again.body.terminal, started.body.terminal);
      // Not a session.
      assert.equal((await call(server, 'GET', `/v1/sessions/${started.body.terminal}`, { token: DEVICE })).status, 404);

      const socket = await terminal(server, started.body.terminal);
      const { screen, texts, code } = await readToClose(socket);
      assert.match(screen, /claude auth login/);
      assert.match(screen, /Signed in as sam@example.com/);
      assert.deepEqual(texts, ['{"type":"exit"}']);
      assert.equal(code, 1000);

      const status = await call<SignIn>(server, 'GET', signInPath('claude'), { token: DEVICE });
      assert.equal(status.body.running, false);
      const after = await call<AgentAccount[]>(server, 'GET', `/v1/machines/${LAPTOP}/agents`, { token: DEVICE });
      assert.deepEqual(after.body[0], { engine: 'claude', installed: true, signed_in: true, account: 'sam@example.com' });
      const next = await call<SignIn>(server, 'POST', signInPath('claude'), { token: DEVICE });
      assert.equal(next.status, 201, 'an ended one is replaced');
      assert.notEqual(next.body.terminal, started.body.terminal);
    }, FAST));

  it('ends when the person presses Enter', () =>
    withServer(async (server) => {
      const started = await call<SignIn>(server, 'POST', signInPath('codex'), {
        token: DEVICE,
        json: { method: 'device_code' },
      });
      assert.deepEqual(started.body.command, ['codex', 'login', '--device-auth']);
      const socket = await terminal(server, started.body.terminal);
      const first = await socket.next();
      assert.equal(first.type, 'binary');
      socket.sendBinary(Buffer.from('ABCD-1234\r'));
      const { screen, code } = await readToClose(socket);
      assert.match(screen, /ABCD-1234/);
      assert.match(screen, /Signed in/);
      assert.equal(code, 1000);
      await sleep(10);
      const status = await call<SignIn>(server, 'GET', signInPath('codex'), { token: DEVICE });
      assert.equal(status.body.running, false);
    }, SLOW));

  it('says why a sign-in cannot start', () =>
    withServer(async (server) => {
      const missing = await call<ApiError>(server, 'POST', signInPath('opencode'), { token: DEVICE });
      assert.equal(missing.status, 409);
      const device = await call<ApiError>(server, 'POST', signInPath('claude'), { token: DEVICE, json: { method: 'device_code' } });
      assert.equal(device.status, 400);
      const extra = await call<ApiError>(server, 'POST', signInPath('claude'), { token: DEVICE, json: { token: 'x' } });
      assert.equal(extra.status, 400);
      assert.equal((await call(server, 'POST', signInPath('gemini'), { token: DEVICE })).status, 404);
      assert.equal((await call(server, 'GET', signInPath('claude'), { token: DEVICE })).status, 404);
    }, SLOW));
});
