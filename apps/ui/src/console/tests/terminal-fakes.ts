// Fakes for the terminal's tests: a socket the test drives from the hub's side, and the page's
// visibility and network state.

import type { ApiError, SocketClose, TransportSocket } from '../../data/index.ts';
import type { Environment } from '../terminal/socket.ts';

const encoder = new TextEncoder();

/** A transport socket (what `useOpenSocket()` opens), driven from the hub's side by the test. */
export class FakeSocket implements TransportSocket {
  onopen: (() => void) | null = null;
  onmessage: ((message: { data: unknown }) => void) | null = null;
  onclose: ((close?: SocketClose) => void) | null = null;
  onerror: (() => void) | null = null;
  bufferedAmount = 0;
  readonly sent: (string | ArrayBuffer | Uint8Array)[] = [];
  closedWith: { code: number | undefined; reason: string | undefined } | undefined;
  readonly path: string;

  constructor(path: string) {
    this.path = path;
  }

  send(data: string | ArrayBuffer | Uint8Array): void {
    this.sent.push(data);
  }

  close(code?: number, reason?: string): void {
    this.closedWith = { code, reason };
  }

  get query(): URLSearchParams {
    return new URLSearchParams(this.path.split('?')[1]);
  }

  get from(): number {
    return Number(this.query.get('from'));
  }

  // The hub's side.
  open(): void {
    this.onopen?.();
  }

  output(bytes: Uint8Array | string): void {
    const data = typeof bytes === 'string' ? encoder.encode(bytes) : bytes;
    // As a browser delivers it with binaryType 'arraybuffer'.
    this.onmessage?.({ data: data.buffer.slice(data.byteOffset, data.byteOffset + data.byteLength) });
  }

  text(frame: unknown): void {
    this.onmessage?.({ data: JSON.stringify(frame) });
  }

  /** Ends it, as the transport reports it: `error` is why it never opened, when known. */
  drop(code = 1006, reason = '', error?: ApiError): void {
    this.onclose?.({ code, reason, ...(error === undefined ? {} : { error }) });
  }

  /** Text frames sent (control messages), parsed. */
  get controls(): unknown[] {
    return this.sent.filter((d): d is string => typeof d === 'string').map((d) => JSON.parse(d));
  }

  /** Binary frames sent (keystrokes), as one byte array. */
  get keys(): number[] {
    return this.sent.filter((d) => typeof d !== 'string').flatMap((d) => [...new Uint8Array(d)]);
  }
}

/** A factory that records every socket it opens. */
export function fakeSockets() {
  const sockets: FakeSocket[] = [];
  return {
    sockets,
    factory: (path: string) => {
      const socket = new FakeSocket(path);
      sockets.push(socket);
      return socket;
    },
    last(): FakeSocket {
      const socket = sockets.at(-1);
      if (socket === undefined) throw new Error('no socket opened');
      return socket;
    },
  };
}

export class FakeEnvironment implements Environment {
  state: 'hidden' | 'offline' | undefined = undefined;
  readonly #listeners = new Set<() => void>();

  blocked() {
    return this.state;
  }

  subscribe(listener: () => void) {
    this.#listeners.add(listener);
    return () => {
      this.#listeners.delete(listener);
    };
  }

  set(state: 'hidden' | 'offline' | undefined) {
    this.state = state;
    for (const listener of this.#listeners) listener();
  }

  get listeners() {
    return this.#listeners.size;
  }
}
