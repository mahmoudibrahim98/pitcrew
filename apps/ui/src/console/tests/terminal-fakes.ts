// Fakes for the terminal's tests: a socket the test drives from the hub's side, and the page's
// visibility and network state.

import type { CloseInfo, Environment, TerminalSocketLike } from '../terminal/socket.ts';

const encoder = new TextEncoder();

export class FakeSocket implements TerminalSocketLike {
  onopen: (() => void) | null = null;
  onmessage: ((message: { data: unknown }) => void) | null = null;
  onclose: ((event: CloseInfo) => void) | null = null;
  onerror: (() => void) | null = null;
  bufferedAmount = 0;
  readonly sent: (string | Uint8Array)[] = [];
  closedWith: { code: number | undefined; reason: string | undefined } | undefined;
  readonly path: string;

  constructor(path: string) {
    this.path = path;
  }

  send(data: string | Uint8Array): void {
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

  drop(code = 1006, reason = '', status?: number): void {
    this.onclose?.({ code, reason, ...(status === undefined ? {} : { status }) });
  }

  /** Text frames sent (control messages), parsed. */
  get controls(): unknown[] {
    return this.sent.filter((d): d is string => typeof d === 'string').map((d) => JSON.parse(d));
  }

  /** Binary frames sent (keystrokes), as one byte array. */
  get keys(): number[] {
    return this.sent.filter((d): d is Uint8Array => typeof d !== 'string').flatMap((d) => [...d]);
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
