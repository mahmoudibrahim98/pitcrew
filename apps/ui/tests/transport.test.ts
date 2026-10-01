// @vitest-environment happy-dom
// The browser transport's own behaviour (the desktop transport is covered in gateway.test.ts).

import { describe, expect, it } from 'vitest';
import { browserTransport } from '../src/data/transport.ts';
import { fakeSockets } from './helpers.ts';

describe('browser transport sockets', () => {
  it('reports the underlying socket’s own bufferedAmount, live', () => {
    const fake = fakeSockets();
    const transport = browserTransport({ baseUrl: 'http://hub.localhost', socket: fake.factory });
    const socket = transport.openSocket('/v1/sessions/S1/terminal?cols=80&rows=24');
    const [raw] = fake.sockets;
    if (raw === undefined) throw new Error('no socket');

    expect(socket.bufferedAmount).toBe(0);
    raw.bufferedAmount = 4096;
    expect(socket.bufferedAmount).toBe(4096);
    raw.bufferedAmount = 0;
    expect(socket.bufferedAmount).toBe(0);
  });

  it('defaults to 0 for a raw socket with no bufferedAmount of its own', () => {
    const transport = browserTransport({
      baseUrl: 'http://hub.localhost',
      socket: () => ({ onmessage: null, onclose: null, onerror: null, send: () => {}, close: () => {} }),
    });
    expect(transport.openSocket('/v1/stream').bufferedAmount).toBe(0);
  });
});
