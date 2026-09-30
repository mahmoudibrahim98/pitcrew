import assert from 'node:assert/strict';
import { randomBytes } from 'node:crypto';
import { describe, it } from 'node:test';
import type { RunningServer } from '../src/server.ts';
import { DEVICE, ID, call, withServer } from './helpers.ts';
import { Refused, TestSocket, type Message } from './ws-client.ts';

function terminal(server: RunningServer, session: string, query = '?cols=100&rows=30'): Promise<TestSocket> {
  return TestSocket.connect(`${server.url}/v1/sessions/${session}/terminal${query}`, [
    'pitcrew.v1',
    `pitcrew.bearer.${DEVICE}`,
  ]);
}

/** Reads the canned replay: binary frames up to the one that ends with the prompt. */
async function readReplay(socket: TestSocket): Promise<string> {
  let screen = '';
  while (!screen.endsWith('> ')) {
    const message = await socket.next();
    assert.equal(message.type, 'binary');
    screen += message.type === 'binary' ? message.data.toString('utf8') : '';
  }
  return screen;
}

async function nextBinary(socket: TestSocket): Promise<Buffer> {
  const message = await socket.next();
  assert.equal(message.type, 'binary');
  return message.type === 'binary' ? message.data : Buffer.alloc(0);
}

async function closeCode(socket: TestSocket): Promise<number> {
  let message: Message;
  do {
    message = await socket.next();
  } while (message.type !== 'close');
  return message.code;
}

describe('terminal socket', () => {
  it('replays a canned screen, echoes keystrokes and accepts a resize', () =>
    withServer(async (server) => {
      const socket = await terminal(server, ID.ses1);
      assert.equal(socket.protocol, 'pitcrew.v1');
      const screen = await readReplay(socket);
      assert.match(screen, /\x1b\[/, 'the replay is ANSI');
      assert.match(screen, /Draft method section/);
      socket.sendText(JSON.stringify({ type: 'resize', cols: 120, rows: 40 }));
      socket.sendBinary(Buffer.from('ls\r'));
      assert.equal((await nextBinary(socket)).toString('utf8'), 'ls\r');
      socket.close();
      assert.equal(await closeCode(socket), 1000);
    }));

  it('echoes payloads that need 16-bit and 64-bit lengths', () =>
    withServer(async (server) => {
      const socket = await terminal(server, ID.ses1);
      await readReplay(socket);
      for (const size of [125, 126, 300, 65535, 65536, 70000]) {
        const data = randomBytes(size);
        socket.sendBinary(data);
        assert.deepEqual(await nextBinary(socket), data, `${size} bytes`);
      }
      socket.close();
    }));

  it('puts fragmented messages back together and answers pings', () =>
    withServer(async (server) => {
      const socket = await terminal(server, ID.ses1);
      await readReplay(socket);
      socket.sendFrame(0x2, Buffer.from('ab'), { fin: false });
      socket.sendFrame(0x9, Buffer.from('ping!'));
      socket.sendFrame(0x0, Buffer.from('cd'));
      const pong = await socket.next();
      assert.deepEqual(pong, { type: 'pong', data: Buffer.from('ping!') });
      assert.equal((await nextBinary(socket)).toString('utf8'), 'abcd');
      socket.close();
    }));

  it('closes with 1002 on an unmasked client frame', () =>
    withServer(async (server) => {
      const socket = await terminal(server, ID.ses1);
      await readReplay(socket);
      socket.sendFrame(0x2, Buffer.from('plain'), { mask: false });
      assert.equal(await closeCode(socket), 1002);
    }));

  it('closes with 1009 on a frame over 1 MiB, before its payload arrives', () =>
    withServer(async (server) => {
      const socket = await terminal(server, ID.ses1);
      await readReplay(socket);
      const head = Buffer.alloc(14);
      head.writeUInt8(0x82, 0);
      head.writeUInt8(0x80 | 127, 1);
      head.writeBigUInt64BE(BigInt(2 * 1024 * 1024), 2);
      socket.sendRaw(head);
      assert.equal(await closeCode(socket), 1009);
    }));

  it('ignores unknown control messages and closes with 1007 on malformed JSON', () =>
    withServer(async (server) => {
      const socket = await terminal(server, ID.ses1);
      await readReplay(socket);
      socket.sendText('{"type":"from_a_newer_client"}');
      socket.sendBinary(Buffer.from('still here'));
      assert.equal((await nextBinary(socket)).toString('utf8'), 'still here');
      socket.sendText('{"type":"resize",');
      assert.equal(await closeCode(socket), 1007);
    }));

  it('tells long sessions that the replay was truncated', () =>
    withServer(async (server) => {
      const socket = await terminal(server, ID.ses2);
      const first = await socket.nextJson<{ type: string; from: number }>();
      assert.equal(first.type, 'truncated');
      assert.equal(typeof first.from, 'number');
      await readReplay(socket);
      socket.close();
    }));

  it('sends exit and closes when the session ends', () =>
    withServer(async (server) => {
      const socket = await terminal(server, ID.ses4);
      await readReplay(socket);
      const ended = await call(server, 'POST', `/v1/sessions/${ID.ses4}/end`, {
        token: DEVICE,
        json: { mode: 'kill' },
      });
      assert.equal(ended.status, 204);
      assert.deepEqual(await socket.nextJson(), { type: 'exit' });
      assert.equal(await closeCode(socket), 1000);
    }));

  it('refuses unknown sessions, unreachable machines and sessions without a terminal', async () => {
    await withServer(async (server) => {
      const status = async (session: string): Promise<number> => {
        try {
          (await terminal(server, session)).destroy();
          return 101;
        } catch (error) {
          assert.ok(error instanceof Refused);
          return error.status;
        }
      };
      assert.equal(await status('01JB000000000000000SES0099'), 404);
      assert.equal(await status(ID.ses5), 503);
      assert.equal(await status(ID.ses6), 404);
    });
  });
});
