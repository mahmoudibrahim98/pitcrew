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
    answer: (list: GatewayWorkspace[]) => answer(list),
    refuse: (error: unknown) => refuse(error),
    reads: () => reads,
    listening: () => listener !== undefined,
  };
}

const settle = () => new Promise((done) => setTimeout(done, 0));

let registry: Workspaces | undefined;

afterEach(() => {
  registry?.stop();
  registry = undefined;
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
