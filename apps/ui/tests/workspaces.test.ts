// The desktop app's workspace registry, against a gateway stand-in (no Tauri): the list follows
// `gateway://workspaces`, each workspace's data is its own, and a workspace that becomes ready
// again reconnects at once.

import { afterEach, describe, expect, it, vi } from 'vitest';
import { Workspaces } from '../src/data/desktop.tsx';
import type { SocketClose, Transport, TransportSocket } from '../src/data/transport.ts';
import type { Gateway, GatewayWorkspace } from '../src/data/workspaces.tsx';

const A: GatewayWorkspace = { id: 'WS-A', name: 'Alpha Lab', kind: 'local', state: 'ready' };
const B: GatewayWorkspace = { id: 'WS-B', name: 'Beta Lab', kind: 'remote', state: 'ready' };

class StubSocket implements TransportSocket {
  onopen: (() => void) | null = null;
  onmessage: ((message: { data: unknown }) => void) | null = null;
  onclose: ((close?: SocketClose) => void) | null = null;
  onerror: (() => void) | null = null;
  bufferedAmount = 0;
  closed = false;
  readonly path: string;
  constructor(path: string) {
    this.path = path;
  }
  send(): void {}
  close(): void {
    this.closed = true;
  }
}

function stubGateway() {
  let listener: ((list: GatewayWorkspace[]) => void) | undefined;
  let navigateListener: ((target: unknown) => void) | undefined;
  let answer: (list: GatewayWorkspace[]) => void = () => {};
  let refuse: (error: unknown) => void = () => {};
  let reads = 0;
  const sockets: { workspace: string; socket: StubSocket }[] = [];
  const transports: Transport[] = [];
  const gateway: Gateway = {
    workspaces: () => {
      reads += 1;
      return new Promise((resolve, reject) => {
        answer = resolve;
        refuse = reject;
      });
    },
    onWorkspaces: async (next) => {
      listener = next;
      return () => (listener = undefined);
    },
    onNavigate: async (next) => {
      navigateListener = next;
      return () => (navigateListener = undefined);
    },
    transport: (id, name) => {
      const transport: Transport = {
        kind: 'desktop',
        get label() {
          return name();
        },
        request: () => Promise.reject(new Error('no requests here')),
        openSocket: (path) => {
          const socket = new StubSocket(path);
          sockets.push({ workspace: id, socket });
          return socket;
        },
      };
      transports.push(transport);
      return transport;
    },
  };
  return {
    gateway,
    sockets,
    transports,
    emit: (list: GatewayWorkspace[]) => listener?.(list),
    /** As the gateway emits `gateway://navigate`, from a deep link or a notification click. */
    navigate: (target: unknown) => navigateListener?.(target),
    answer: (list: GatewayWorkspace[]) => answer(list),
    refuse: (error: unknown) => refuse(error),
    reads: () => reads,
    listening: () => listener !== undefined,
    navigateListening: () => navigateListener !== undefined,
  };
}

const settle = () => new Promise((done) => setTimeout(done, 0));

let registry: Workspaces | undefined;

afterEach(() => {
  registry?.stop();
  registry = undefined;
  vi.useRealTimers();
});

describe('the workspace registry', () => {
  it('subscribes before it reads the list; an event heard meanwhile wins over the answer', async () => {
    const stub = stubGateway();
    registry = new Workspaces(stub.gateway);
    registry.start();
    await settle();
    expect(stub.listening()).toBe(true);
    stub.emit([A, B]);
    stub.answer([A]);
    await settle();
    expect(registry.store.getState().list).toEqual([A, B]);
    stub.emit([B]);
    expect(registry.store.getState().list).toEqual([B]);
  });

  it('subscribes to gateway://navigate before it reads the list too, and holds a target that arrives first', async () => {
    const stub = stubGateway();
    registry = new Workspaces(stub.gateway);
    const received: [unknown, readonly GatewayWorkspace[]][] = [];
    void registry.onNavigate((target, workspaces) => received.push([target, workspaces]));
    registry.start();
    await settle();
    // Already subscribed by the time the list is read: the gateway relies on this to hand over a
    // launch-time deep link it held until this first `gateway_workspaces()` call.
    expect(stub.navigateListening()).toBe(true);

    // Arrives before the list is known: held, not dropped and not delivered with a stale (empty) list.
    stub.navigate({ workspace: A.id, kind: 'inbox' });
    expect(received).toEqual([]);
    stub.answer([A]);
    await settle();
    expect(received).toEqual([[{ workspace: A.id, kind: 'inbox' }, [A]]]);
  });

  it('delivers a gateway://navigate target at once, with the list, once the list is already known', async () => {
    const stub = stubGateway();
    registry = new Workspaces(stub.gateway);
    registry.start();
    await settle();
    stub.answer([A, B]);
    await settle();

    const received: [unknown, readonly GatewayWorkspace[]][] = [];
    void registry.onNavigate((target, workspaces) => received.push([target, workspaces]));
    stub.navigate({ workspace: B.id, kind: 'task', id: 'PAP-4' });
    expect(received).toEqual([[{ workspace: B.id, kind: 'task', id: 'PAP-4' }, [A, B]]]);
  });

  it('drops a held gateway://navigate target once pendingNavigateMs passes without the list', async () => {
    vi.useFakeTimers();
    const stub = stubGateway();
    registry = new Workspaces(stub.gateway, { pendingNavigateMs: 1_000 });
    const received: unknown[] = [];
    void registry.onNavigate((target) => received.push(target));
    registry.start();
    await vi.advanceTimersByTimeAsync(0);
    expect(stub.navigateListening()).toBe(true);

    stub.navigate({ workspace: A.id, kind: 'inbox' });
    await vi.advanceTimersByTimeAsync(1_001);
    // Expired: the list arriving late must not deliver a stale target.
    stub.answer([A]);
    await vi.advanceTimersByTimeAsync(0);
    expect(received).toEqual([]);
  });

  it('reads a list it could not read again, with back-off, until it has one', async () => {
    const stub = stubGateway();
    registry = new Workspaces(stub.gateway, { listBackoff: { initialMs: 20, maxMs: 40 } });
    registry.start();
    await settle();
    expect(stub.reads()).toBe(1);
    stub.refuse({ code: 'internal', message: 'The gateway is not ready.' });
    await vi.waitFor(() => expect(registry?.store.getState().error).toBe('The gateway is not ready.'));
    expect(registry.store.getState().list).toBeUndefined();

    // No event will come (the list did not change): the registry asks again by itself.
    await vi.waitFor(() => expect(stub.reads()).toBe(2));
    stub.refuse(new Error('Still not ready.'));
    await vi.waitFor(() => expect(registry?.store.getState().error).toBe('Still not ready.'));
    await vi.waitFor(() => expect(stub.reads()).toBe(3));
    stub.answer([A]);
    await vi.waitFor(() => expect(registry?.store.getState()).toEqual({ list: [A], error: undefined }));

    // Known now: no more reads, and retry() does nothing.
    registry.retry();
    await new Promise((done) => setTimeout(done, 100));
    expect(stub.reads()).toBe(3);
  });

  it('reads the list again at once on retry(), while it is unknown', async () => {
    const stub = stubGateway();
    registry = new Workspaces(stub.gateway, { listBackoff: { initialMs: 60_000, maxMs: 60_000 } });
    registry.start();
    await settle();
    stub.refuse(new Error('The gateway is not ready.'));
    await vi.waitFor(() => expect(registry?.store.getState().error).toBe('The gateway is not ready.'));
    registry.retry();
    registry.retry(); // one read at a time
    expect(stub.reads()).toBe(2);
    stub.answer([A, B]);
    await vi.waitFor(() => expect(registry?.store.getState().list).toEqual([A, B]));
  });

  it("names a workspace by its current name in the stream's messages", async () => {
    const stub = stubGateway();
    registry = new Workspaces(stub.gateway);
    registry.start();
    await settle();
    stub.answer([A]);
    await settle();
    registry.data(A);
    expect(stub.transports[0]?.label).toBe('Alpha Lab');
    stub.emit([{ ...A, name: 'Alpha Lab, renamed' }]);
    expect(stub.transports[0]?.label).toBe('Alpha Lab, renamed');
  });

  it("keeps each workspace's data apart, and drops a removed workspace's", async () => {
    const stub = stubGateway();
    registry = new Workspaces(stub.gateway);
    registry.start();
    await settle();
    stub.answer([A, B]);
    await settle();

    const alpha = registry.data(A);
    const beta = registry.data(B);
    expect(alpha.queryClient).not.toBe(beta.queryClient);
    expect(alpha.api).not.toBe(beta.api);
    expect(registry.data(A)).toBe(alpha);

    registry.open(A.id);
    registry.open(B.id);
    expect(stub.sockets.map((s) => [s.workspace, s.socket.path])).toEqual([
      [A.id, '/v1/stream'],
      [B.id, '/v1/stream'],
    ]);
    alpha.queryClient.setQueryData(['projects', 'list'], ['alpha']);

    stub.emit([B]);
    expect(stub.sockets[0]?.socket.closed).toBe(true);
    expect(alpha.queryClient.getQueryData(['projects', 'list'])).toBeUndefined();
    expect(registry.data(A)).not.toBe(alpha);
    expect(beta.queryClient.getQueryData(['projects', 'list'])).toBeUndefined();
  });

  // `backgroundMs` is kept well under the stream's own 60 s silence timeout here (default,
  // not configurable through `Workspaces`), so these fake-timer advances exercise only the
  // background-close timer, not an unrelated reconnect from a quiet socket.
  const BACKGROUND_MS = 1_000;

  it('closes a backgrounded workspace’s stream after it is out of view, and resumes it with since', async () => {
    vi.useFakeTimers();
    const stub = stubGateway();
    registry = new Workspaces(stub.gateway, { backgroundMs: BACKGROUND_MS });
    registry.start();
    await vi.advanceTimersByTimeAsync(0);
    stub.answer([A]);
    await vi.advanceTimersByTimeAsync(0);

    registry.data(A);
    registry.open(A.id);
    expect(stub.sockets).toHaveLength(1);
    // The stream learns a revision, so the next connection can resume from it.
    stub.sockets[0]?.socket.onmessage?.({ data: JSON.stringify({ type: 'hello', rev: 5, log: 'LOG-A' }) });

    registry.leave(A.id);
    await vi.advanceTimersByTimeAsync(BACKGROUND_MS - 100);
    expect(stub.sockets[0]?.socket.closed).toBe(false);

    await vi.advanceTimersByTimeAsync(200);
    expect(stub.sockets[0]?.socket.closed).toBe(true);
    expect(stub.sockets).toHaveLength(1);

    // Back into view: a new stream, resumed from where the old one left off.
    registry.open(A.id);
    expect(stub.sockets).toHaveLength(2);
    expect(stub.sockets[1]?.socket.path).toBe('/v1/stream?since=5');
  });

  it('cancels the background close when a workspace comes back into view first', async () => {
    vi.useFakeTimers();
    const stub = stubGateway();
    registry = new Workspaces(stub.gateway, { backgroundMs: BACKGROUND_MS });
    registry.start();
    await vi.advanceTimersByTimeAsync(0);
    stub.answer([A]);
    await vi.advanceTimersByTimeAsync(0);

    registry.data(A);
    registry.open(A.id);
    registry.leave(A.id);
    await vi.advanceTimersByTimeAsync(BACKGROUND_MS / 2);
    registry.open(A.id);
    await vi.advanceTimersByTimeAsync(BACKGROUND_MS * 2);

    // Never closed, and still the one stream throughout.
    expect(stub.sockets[0]?.socket.closed).toBe(false);
    expect(stub.sockets).toHaveLength(1);
  });

  it('does not background a workspace that was removed before the timeout fired', async () => {
    vi.useFakeTimers();
    const stub = stubGateway();
    registry = new Workspaces(stub.gateway, { backgroundMs: BACKGROUND_MS });
    registry.start();
    await vi.advanceTimersByTimeAsync(0);
    stub.answer([A]);
    await vi.advanceTimersByTimeAsync(0);

    registry.data(A);
    registry.open(A.id);
    registry.leave(A.id);
    stub.emit([]); // removed from the list entirely
    // The pending close must not throw reaching for data that is already gone.
    await vi.advanceTimersByTimeAsync(BACKGROUND_MS + 100);
    expect(stub.sockets[0]?.socket.closed).toBe(true); // closed as part of removal, not the timer
  });

  it('reconnects a workspace at once when the gateway says it is ready again', async () => {
    const stub = stubGateway();
    registry = new Workspaces(stub.gateway);
    registry.start();
    await settle();
    stub.answer([A]);
    await settle();
    registry.data(A);
    registry.open(A.id);
    expect(stub.sockets).toHaveLength(1);

    // The connection breaks; the gateway says the workspace is unreachable.
    stub.sockets[0]?.socket.onclose?.({ code: 1006, reason: '' });
    stub.emit([{ ...A, state: 'unreachable' }]);
    expect(stub.sockets).toHaveLength(1);

    // Back: no waiting for the next retry.
    stub.emit([A]);
    expect(stub.sockets).toHaveLength(2);
    expect(stub.sockets[1]?.socket.path).toBe('/v1/stream');
  });
});
