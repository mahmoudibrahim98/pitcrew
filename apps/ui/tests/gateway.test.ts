// @vitest-environment happy-dom
// The desktop transport against a fake gateway that follows docs/build/contracts/desktop-gateway.md.

import { QueryClient } from '@tanstack/react-query';
import { afterEach, beforeEach, describe, expect, it, vi } from 'vitest';
import { ApiError, createApi } from '../src/data/api.ts';
import { GatewayError, type GatewayErrorCode } from '../src/data/errors.ts';
import { gatewayTransport, type GatewayRequest, type GatewayResponse } from '../src/data/gateway.ts';
import { createLive, type Live } from '../src/data/live.ts';
import { StreamClient } from '../src/data/stream.ts';
import type { SocketClose } from '../src/data/transport.ts';
import type { Event } from '../src/data/types.ts';
import type { GatewayWorkspace } from '../src/data/workspaces.tsx';
import { FakeGateway } from './fake-gateway.ts';

const WS = '01JB000000000000000WSP0001';
const LAB: GatewayWorkspace = { id: WS, name: 'Demo Lab', kind: 'local', state: 'ready' };

let gateway: FakeGateway;

beforeEach(() => {
  gateway = new FakeGateway([LAB]).install();
});

afterEach(() => {
  gateway.uninstall();
});

async function failure(promise: Promise<unknown>): Promise<unknown> {
  try {
    await promise;
  } catch (error) {
    return error;
  }
  throw new Error('expected a failure');
}

const json = (status: number, body: unknown) => ({ status, contentType: 'application/json', body: JSON.stringify(body) });

describe('requests through the gateway', () => {
  it('sends method, path with its query, and JSON text, and nothing else', async () => {
    const seen: GatewayRequest[] = [];
    gateway.daemons.set(WS, (req) => {
      seen.push(req);
      return req.method === 'GET' ? json(200, [{ id: 'T1' }]) : json(200, { id: 'T1', status: 'review' });
    });
    const api = createApi({ transport: gatewayTransport(WS) });

    expect(await api.tasks({ status: ['todo', 'review'] })).toEqual([{ id: 'T1' }]);
    expect(await api.moveTask('T1', 'review')).toEqual({ id: 'T1', status: 'review' });

    expect(seen).toEqual([
      { workspace: WS, method: 'GET', path: '/v1/tasks?status=todo&status=review' },
      { workspace: WS, method: 'POST', path: '/v1/tasks/T1/move', body: '{"to":"review"}' },
    ]);
    // One argument, `req`: no headers, no token.
    for (const call of gateway.calls) {
      expect(call.cmd).toBe('gateway_request');
      expect(Object.keys(call.args)).toEqual(['req']);
    }
  });

  it('returns nothing for 204, with an empty body or none', async () => {
    gateway.daemons.set(WS, () => ({ status: 204, body: '' }));
    const api = createApi({ transport: gatewayTransport(WS) });
    await expect(api.interrupt('S1')).resolves.toBeUndefined();
    gateway.daemons.set(WS, () => ({ status: 204, body: null }) as unknown as GatewayResponse);
    await expect(api.interrupt('S1')).resolves.toBeUndefined();
  });

  it("passes the daemon's ApiError through with its status", async () => {
    gateway.daemons.set(WS, (req) =>
      req.path.startsWith('/v1/tasks/')
        ? json(409, { code: 'conflict', message: 'That move is not allowed.' })
        : { status: 502, contentType: 'text/html', body: '<h1>Bad gateway</h1>' },
    );
    const api = createApi({ transport: gatewayTransport(WS) });

    const conflict = await failure(api.moveTask('T1', 'done'));
    expect(conflict).toBeInstanceOf(ApiError);
    expect(conflict).not.toBeInstanceOf(GatewayError);
    expect(conflict).toMatchObject({ code: 'conflict', status: 409, message: 'That move is not allowed.' });
    expect(await failure(api.projects())).toMatchObject({ code: 'internal', status: 502 });
  });

  it('turns each gateway failure into an ApiError the UI already handles', async () => {
    gateway.workspaces = [
      LAB,
      { id: 'W-PAIR', name: 'Paired once', kind: 'remote', state: 'needs_pairing', detail: 'The token was revoked.' },
      { id: 'W-DOWN', name: 'hpc-login', kind: 'remote', state: 'unreachable', detail: 'SSH timed out.' },
    ];
    gateway.daemons.set(WS, (req) =>
      req.path === '/v1/me' ? Promise.reject({ code: 'internal', message: 'The tunnel broke.' }) : json(200, {}),
    );
    const lab = createApi({ transport: gatewayTransport(WS) });
    const expectations: [Promise<unknown>, GatewayErrorCode, ApiError['code']][] = [
      [createApi({ transport: gatewayTransport('W-NONE') }).me(), 'unknown_workspace', 'not_found'],
      [createApi({ transport: gatewayTransport('W-PAIR') }).me(), 'needs_pairing', 'unauthorized'],
      [createApi({ transport: gatewayTransport('W-DOWN') }).me(), 'unreachable', 'unavailable'],
      [lab.request('GET', '/v1/../secrets'), 'invalid', 'invalid'],
      [lab.request('POST', '/v1/tasks', { body: { title: 'x'.repeat(1024 * 1024) } }), 'too_large', 'invalid'],
      [lab.me(), 'internal', 'internal'],
    ];
    for (const [promise, gatewayCode, apiCode] of expectations) {
      const error = await failure(promise);
      expect(error).toBeInstanceOf(GatewayError);
      expect(error).toBeInstanceOf(ApiError);
      // The daemon never answered: no made-up HTTP status.
      expect(error).toMatchObject({ gateway: gatewayCode, code: apiCode, status: 0 });
    }
    // `unreachable` is the same state as a network failure in a browser.
    const offline = await failure(
      createApi({ baseUrl: 'http://hub.localhost', fetch: () => Promise.reject(new TypeError('fetch failed')) }).me(),
    );
    const down = await failure(createApi({ transport: gatewayTransport('W-DOWN') }).me());
    expect(down).toMatchObject({ code: (offline as ApiError).code, status: (offline as ApiError).status });
    expect(down).toMatchObject({ message: 'SSH timed out.' });
  });

  it('reads a rejection that is not a GatewayError as internal', async () => {
    gateway.daemons.set(WS, () => Promise.reject('command gateway_request not allowed'));
    const error = await failure(createApi({ transport: gatewayTransport(WS) }).me());
    expect(error).toMatchObject({ gateway: 'internal', code: 'internal', status: 0, message: 'command gateway_request not allowed' });
  });

  it('gives up on an aborted request', async () => {
    let answer: (value: ReturnType<typeof json>) => void = () => {};
    gateway.daemons.set(WS, () => new Promise((resolve) => (answer = resolve)));
    const controller = new AbortController();
    const pending = createApi({ transport: gatewayTransport(WS) }).projects(controller.signal);
    await vi.waitFor(() => expect(gateway.calls).toHaveLength(1));
    controller.abort();
    const error = await failure(pending);
    expect(error).toMatchObject({ name: 'AbortError' });
    answer(json(200, []));
  });
});

describe('sockets through the gateway', () => {
  it('opens with workspace, path and a channel, and delivers frames in order, binary as ArrayBuffer, close last', async () => {
    const socket = gatewayTransport(WS).openSocket('/v1/sessions/S1/terminal?cols=80&rows=24');
    const got: unknown[] = [];
    const closes: (SocketClose | undefined)[] = [];
    socket.onmessage = (message) => got.push(message.data);
    socket.onclose = (close) => closes.push(close);
    let opened = false;
    socket.onopen = () => (opened = true);
    await vi.waitFor(() => expect(opened).toBe(true));

    const open = gateway.calls.find((c) => c.cmd === 'gateway_socket_open');
    expect(open?.args).toMatchObject({ workspace: WS, path: '/v1/sessions/S1/terminal?cols=80&rows=24' });
    expect(Object.keys(open?.args ?? {}).sort()).toEqual(['events', 'path', 'workspace']);

    const [fake] = gateway.sockets;
    if (fake === undefined) throw new Error('no socket');
    fake.json({ type: 'truncated', from: 0 });
    fake.binary([104, 105]);
    // Tauri may hand messages over out of order; the channel puts them back.
    fake.deliverAt(3, { type: 'text', data: '{"type":"exit"}' });
    fake.deliverAt(2, new Uint8Array([33]).buffer);
    fake.deliverAt(4, { type: 'close', code: 1000, reason: 'exited' });
    fake.deliverAt(5, { type: 'text', data: 'after the close' });

    expect(got).toHaveLength(4);
    expect(got[0]).toBe('{"type":"truncated","from":0}');
    expect(got[1]).toBeInstanceOf(ArrayBuffer);
    expect([...new Uint8Array(got[1] as ArrayBuffer)]).toEqual([104, 105]);
    expect([...new Uint8Array(got[2] as ArrayBuffer)]).toEqual([33]);
    expect(got[3]).toBe('{"type":"exit"}');
    expect(closes).toEqual([{ code: 1000, reason: 'exited' }]);
  });

  it('ends with 1006 and the reason when the gateway cannot open it', async () => {
    gateway.workspaces = [{ ...LAB, state: 'needs_pairing', detail: 'Pair it again.' }];
    const socket = gatewayTransport(WS).openSocket('/v1/stream');
    const events: string[] = [];
    let close: SocketClose | undefined;
    socket.onerror = () => events.push('error');
    socket.onclose = (c) => {
      events.push('close');
      close = c;
    };
    await vi.waitFor(() => expect(events).toEqual(['error', 'close']));
    expect(close).toMatchObject({ code: 1006, reason: 'Pair it again.' });
    expect(close?.error).toBeInstanceOf(GatewayError);
    expect(close?.error).toMatchObject({ gateway: 'needs_pairing' });

    gateway.workspaces = [LAB];
    const other = gatewayTransport(WS).openSocket('/v1/tasks');
    let refused: SocketClose | undefined;
    other.onclose = (c) => (refused = c);
    await vi.waitFor(() => expect(refused?.error).toMatchObject({ gateway: 'invalid' }));
  });

  it('sends text and bytes one at a time, in order, then closes with the code', async () => {
    const socket = gatewayTransport(WS).openSocket('/v1/sessions/S1/terminal?cols=80&rows=24');
    const closes: (SocketClose | undefined)[] = [];
    socket.onclose = (close) => closes.push(close);
    let opened = false;
    socket.onopen = () => (opened = true);
    // Before it opens: this waits for it.
    socket.send('{"type":"resize","cols":100,"rows":30}');
    await vi.waitFor(() => expect(opened).toBe(true));
    // While the first is in flight, the rest wait their turn.
    socket.send(new Uint8Array([108]));
    socket.send(new Uint8Array([115]));
    socket.send(new Uint8Array([13]).buffer);
    socket.send('{"type":"resize","cols":120,"rows":40}');
    socket.close(1000, 'done');
    socket.send('dropped');
    socket.close(); // idempotent

    await vi.waitFor(() => expect(closes).toHaveLength(1));
    const [fake] = gateway.sockets;
    // Bytes in a row go as one frame; the order holds; the close goes last; nothing after it.
    expect(fake?.sent).toEqual([
      { text: '{"type":"resize","cols":100,"rows":30}' },
      { binary: [108] },
      { binary: [115, 13] },
      { text: '{"type":"resize","cols":120,"rows":40}' },
    ]);
    expect(fake?.closed).toEqual({ code: 1000, reason: 'done', by: 'webview' });
    expect(closes).toEqual([{ code: 1000, reason: 'done' }]);
    const sends = gateway.calls.filter((c) => c.cmd === 'gateway_socket_send');
    for (const send of sends) expect(Object.keys(send.args).length).toBe(2);
    expect(gateway.calls.filter((c) => c.cmd === 'gateway_socket_close')).toEqual([
      { cmd: 'gateway_socket_close', args: { socket: fake?.id, code: 1000, reason: 'done' } },
    ]);
    // The fake holds each send for a tick: the socket never had two out at once.
    expect(fake?.maxInFlight).toBe(1);
  });

  it('ends the socket with 1011 when a send is refused, and says why once', async () => {
    const warn = vi.spyOn(console, 'warn').mockImplementation(() => {});
    const socket = gatewayTransport(WS).openSocket('/v1/sessions/S1/terminal?cols=80&rows=24');
    const closes: (SocketClose | undefined)[] = [];
    socket.onclose = (close) => closes.push(close);
    let opened = false;
    socket.onopen = () => (opened = true);
    await vi.waitFor(() => expect(opened).toBe(true));

    gateway.refuseSends = { code: 'internal', message: 'The pipe broke.' };
    socket.send('{"type":"resize","cols":100,"rows":30}');
    socket.send(new Uint8Array([108, 115]));
    socket.send('{"type":"resize","cols":120,"rows":40}');
    await vi.waitFor(() => expect(closes).toEqual([{ code: 1011, reason: 'send failed' }]));
    socket.send('after the close');

    const [fake] = gateway.sockets;
    // The frames after the refused one are dropped, not sent with a hole before them.
    expect(gateway.calls.filter((c) => c.cmd === 'gateway_socket_send')).toHaveLength(1);
    expect(fake?.sent).toEqual([]);
    expect(gateway.calls.filter((c) => c.cmd === 'gateway_socket_close')).toEqual([
      { cmd: 'gateway_socket_close', args: { socket: fake?.id, code: 1011, reason: 'send failed' } },
    ]);
    expect(warn).toHaveBeenCalledTimes(1);
    expect(String(warn.mock.calls[0]?.[0])).toContain('The pipe broke.');
    warn.mockRestore();
  });

  it('ends the socket here with 1006 when the gateway cannot close it', async () => {
    const warn = vi.spyOn(console, 'warn').mockImplementation(() => {});
    gateway.refuseClose = { code: 'internal', message: 'The gateway is shutting down.' };
    const plain = gatewayTransport(WS).openSocket('/v1/stream');
    const plainCloses: (SocketClose | undefined)[] = [];
    plain.onclose = (close) => plainCloses.push(close);
    plain.close();
    await vi.waitFor(() => expect(plainCloses).toHaveLength(1));
    expect(plainCloses[0]).toMatchObject({ code: 1006, reason: 'The gateway is shutting down.' });
    expect(plainCloses[0]?.error).toMatchObject({ gateway: 'internal' });

    // A refused send, and then a refused close: it still ends, once.
    gateway.refuseSends = { code: 'internal', message: 'The pipe broke.' };
    const broken = gatewayTransport(WS).openSocket('/v1/sessions/S1/terminal?cols=80&rows=24');
    const brokenCloses: (SocketClose | undefined)[] = [];
    broken.onclose = (close) => brokenCloses.push(close);
    broken.send('x');
    await vi.waitFor(() => expect(brokenCloses).toHaveLength(1));
    expect(brokenCloses[0]).toMatchObject({ code: 1006, reason: 'The gateway is shutting down.' });
    broken.close();
    await new Promise((done) => setTimeout(done, 20));
    expect(brokenCloses).toHaveLength(1);
    warn.mockRestore();
  });

  it('delivers frames that arrive before the open command resolves', async () => {
    gateway.holdOpens = true;
    const socket = gatewayTransport(WS).openSocket('/v1/sessions/S1/terminal?cols=80&rows=24');
    const got: unknown[] = [];
    const events: string[] = [];
    socket.onmessage = (message) => got.push(message.data);
    socket.onopen = () => events.push('open');
    const [fake] = gateway.sockets;
    if (fake === undefined) throw new Error('no socket');
    fake.json({ type: 'truncated', from: 0 });
    fake.binary([1, 2]);
    expect(got).toHaveLength(2);
    expect(got[1]).toBeInstanceOf(ArrayBuffer);
    expect(events).toEqual([]);

    socket.send('{"type":"resize","cols":90,"rows":20}');
    fake.resolveOpen();
    await vi.waitFor(() => expect(fake.sent).toEqual([{ text: '{"type":"resize","cols":90,"rows":20}' }]));
    expect(events).toEqual(['open']);
  });

  it('ends at a close that arrives before the open command resolves, and never opens', async () => {
    gateway.holdOpens = true;
    const socket = gatewayTransport(WS).openSocket('/v1/stream');
    const events: string[] = [];
    socket.onopen = () => events.push('open');
    socket.onclose = (close) => events.push(`close ${close?.code}`);
    const [fake] = gateway.sockets;
    fake?.close(1001, 'shutting down');
    expect(events).toEqual(['close 1001']);
    socket.send('dropped');
    fake?.resolveOpen();
    await new Promise((done) => setTimeout(done, 20));
    expect(events).toEqual(['close 1001']);
    expect(gateway.calls.filter((c) => c.cmd === 'gateway_socket_send' || c.cmd === 'gateway_socket_close')).toEqual([]);
  });

  it('ends with 1006 when the open answer has no socket id', async () => {
    gateway.openAnswer = { socket: 'seven' };
    const socket = gatewayTransport(WS).openSocket('/v1/stream');
    let close: SocketClose | undefined;
    socket.onclose = (c) => (close = c);
    await vi.waitFor(() => expect(close).toMatchObject({ code: 1006, error: { gateway: 'internal' } }));
  });

  it('ignores a message of an unknown shape, and says so once', async () => {
    const warn = vi.spyOn(console, 'warn').mockImplementation(() => {});
    const socket = gatewayTransport(WS).openSocket('/v1/sessions/S1/terminal?cols=80&rows=24');
    const got: unknown[] = [];
    socket.onmessage = (message) => got.push(message.data);
    const [fake] = gateway.sockets;
    fake?.deliver([104, 105]);
    fake?.deliver({ type: 'text', data: 42 });
    fake?.text('still fine');
    expect(got).toEqual(['still fine']);
    expect(warn).toHaveBeenCalledTimes(1);
    warn.mockRestore();
  });

  it('closed before it opened: sends nothing, and closes once open', async () => {
    const socket = gatewayTransport(WS).openSocket('/v1/stream');
    const closes: (SocketClose | undefined)[] = [];
    socket.onclose = (close) => closes.push(close);
    socket.send('{"type":"ignored"}');
    socket.close();
    await vi.waitFor(() => expect(closes).toEqual([{ code: 1000, reason: '' }]));
    expect(gateway.sockets[0]?.sent).toEqual([]);
    expect(gateway.calls.filter((c) => c.cmd === 'gateway_socket_close')).toEqual([
      { cmd: 'gateway_socket_close', args: { socket: gateway.sockets[0]?.id } },
    ]);
  });
});

function taskMoved(id: string): Event {
  return {
    id,
    at: 0,
    workspace: WS,
    author: 'M',
    body: { type: 'task_moved', data: { task: 'T', from: 'todo', to: 'review', mover: { kind: 'person' } } },
  };
}

describe('the stream through the gateway', () => {
  it('reads frames in order and resumes with since after a 1013 close', async () => {
    const received: string[] = [];
    const stream = new StreamClient({
      transport: gatewayTransport(WS),
      onEvents: (events) => received.push(...events.map((e) => e.id)),
      onReset: () => {},
      backoff: { initialMs: 5, maxMs: 10 },
    });
    stream.start();
    await vi.waitFor(() => expect(gateway.sockets).toHaveLength(1));
    const first = gateway.sockets[0];
    expect(first?.path).toBe('/v1/stream');
    first?.json({ type: 'hello', rev: 5, log: 'LOG-A' });
    first?.json({ type: 'events', from_rev: 6, to_rev: 7, events: [taskMoved('E6'), taskMoved('E7')] });
    expect(stream.status).toBe('live');
    expect(received).toEqual(['E6', 'E7']);

    // Too slow: the gateway closes with 1013, and the stream resumes where it was.
    first?.close(1013, 'too slow');
    await vi.waitFor(() => expect(gateway.sockets).toHaveLength(2));
    const second = gateway.sockets[1];
    expect(second?.path).toBe('/v1/stream?since=7');
    second?.json({ type: 'hello', rev: 8, log: 'LOG-A' });
    second?.json({ type: 'events', from_rev: 8, to_rev: 8, events: [taskMoved('E8')] });
    expect(received).toEqual(['E6', 'E7', 'E8']);
    stream.stop();
    await vi.waitFor(() => expect(second?.closed).toMatchObject({ code: 1000, by: 'webview' }));
  });

  it('says needs_pairing or unreachable from what the gateway said, without probing', async () => {
    const warn = vi.spyOn(console, 'warn').mockImplementation(() => {});
    const lives: Live[] = [];
    const probe = vi.fn(async () => undefined);
    const live = (workspace: string, name: string) => {
      const one = createLive({
        queryClient: new QueryClient(),
        transport: gatewayTransport(workspace, () => name),
        backoff: { initialMs: 1, maxMs: 2 },
        probeAfter: 2,
        probe,
      });
      lives.push(one);
      one.start();
      return one;
    };
    gateway.workspaces = [
      { ...LAB, state: 'needs_pairing' },
      { id: 'W-DOWN', name: 'hpc-login', kind: 'remote', state: 'unreachable' },
    ];
    const pairing = live(WS, 'Demo Lab');
    const down = live('W-DOWN', 'hpc-login');
    await vi.waitFor(() => expect(pairing.store.getState().problem).toBe('needs_pairing'));
    await vi.waitFor(() => expect(down.store.getState().problem).toBe('unreachable'));
    expect(probe).not.toHaveBeenCalled();
    // The warnings carry the gateway's own words.
    const warnings = warn.mock.calls.map((call) => String(call[0]));
    expect(warnings).toContain(
      'pitcrew: the workspace “Demo Lab” needs pairing (Pair this workspace again.); the stream keeps retrying.',
    );
    expect(warnings).toContain(
      'pitcrew: cannot reach the workspace “hpc-login” (The daemon does not answer.); the stream keeps retrying.',
    );

    // Paired again: back to live, and the problem is gone.
    gateway.workspaces = [LAB];
    gateway.onSocket = (socket) => socket.json({ type: 'hello', rev: 1, log: 'LOG-A' });
    pairing.retryNow();
    await vi.waitFor(() => expect(pairing.store.getState()).toMatchObject({ status: 'live', problem: undefined }));
    for (const one of lives) one.stop();
    warn.mockRestore();
  });
});
