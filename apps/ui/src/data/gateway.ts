// The desktop transport: every request and socket goes through the gateway's commands and
// channels, exactly as `docs/build/contracts/desktop-gateway.md` says. The gateway adds the
// workspace's token; nothing here reads, holds or sends one, and the webview sets no headers.
//
// Loaded only in the desktop app, with `desktop.tsx`, by dynamic import, so `@tauri-apps/api`
// stays out of the browser's first chunk. Elsewhere, import from here with `import type` only.

import { Channel, invoke } from '@tauri-apps/api/core';
import { listen } from '@tauri-apps/api/event';
import { GatewayError, toGatewayError } from './errors.ts';
import type { Method, SocketClose, Transport, TransportResponse, TransportSocket } from './transport.ts';
import type { Gateway, GatewayWorkspace } from './workspaces.tsx';

/** Emitted with the whole list whenever it changes. */
export const WORKSPACES_EVENT = 'gateway://workspaces';

export interface GatewayRequest {
  workspace: string;
  method: Method;
  /** `/v1/…`, with an optional `?query`. */
  path: string;
  /** JSON text. */
  body?: string;
}

export interface GatewayResponse {
  status: number;
  contentType?: string;
  body: string;
}

/** Calls a gateway command; a rejection is always a `GatewayError`. */
async function call<T>(command: string, args: Record<string, unknown>): Promise<T> {
  try {
    return await invoke<T>(command, args);
  } catch (reason) {
    throw toGatewayError(reason);
  }
}

export function createGateway(): Gateway {
  return {
    workspaces: () => call<GatewayWorkspace[]>('gateway_workspaces', {}),
    onWorkspaces: (listener) => listen<GatewayWorkspace[]>(WORKSPACES_EVENT, (event) => listener(event.payload)),
    transport: (id, name) => gatewayTransport(id, name),
  };
}

function abortError(signal: AbortSignal): unknown {
  return signal.reason ?? new DOMException('The request was aborted.', 'AbortError');
}

/** `answer`, or the abort: the gateway cannot cancel a request, so a late answer is dropped. */
function abortable<T>(answer: Promise<T>, signal: AbortSignal): Promise<T> {
  return new Promise<T>((resolve, reject) => {
    const onAbort = () => reject(abortError(signal));
    signal.addEventListener('abort', onAbort, { once: true });
    answer.then(
      (value) => {
        signal.removeEventListener('abort', onAbort);
        resolve(value);
      },
      (error: unknown) => {
        signal.removeEventListener('abort', onAbort);
        reject(error);
      },
    );
  });
}

/** A response; an empty body (a 204) may come as `null`. */
function isResponse(value: unknown): value is Omit<GatewayResponse, 'body'> & { body?: string | null } {
  if (typeof value !== 'object' || value === null) return false;
  const { status, body } = value as Record<string, unknown>;
  return typeof status === 'number' && (typeof body === 'string' || body === null || body === undefined);
}

/** The transport for one workspace (its id, as in `/w/$ws`); `name` names it in messages. */
export function gatewayTransport(workspace: string, name: string | (() => string) = workspace): Transport {
  const currentName = typeof name === 'function' ? name : () => name;

  async function request(
    method: Method,
    path: string,
    body?: string,
    signal?: AbortSignal,
  ): Promise<TransportResponse> {
    if (signal?.aborted) throw abortError(signal);
    const req: GatewayRequest = body === undefined ? { workspace, method, path } : { workspace, method, path, body };
    const answer = call<unknown>('gateway_request', { req });
    const res = signal === undefined ? await answer : await abortable(answer, signal);
    if (!isResponse(res)) throw toGatewayError({ code: 'internal', message: `The gateway gave no answer for ${method} ${path}.` });
    return { status: res.status, contentType: res.contentType ?? undefined, body: res.body ?? '' };
  }

  return {
    kind: 'desktop',
    get label() {
      return `the workspace “${currentName()}”`;
    },
    request,
    openSocket: (path) => new GatewaySocket(workspace, path),
  };
}

function openedId(answer: unknown): number | undefined {
  if (typeof answer !== 'object' || answer === null) return undefined;
  const { socket } = answer as { socket?: unknown };
  return typeof socket === 'number' && Number.isInteger(socket) ? socket : undefined;
}

/** Consecutive binary frames are sent as one, up to this size. */
const BATCH_BYTES = 256 * 1024;

function asArrayBuffer(message: unknown): ArrayBuffer | undefined {
  if (message instanceof ArrayBuffer) return message;
  if (Object.prototype.toString.call(message) === '[object ArrayBuffer]') return message as ArrayBuffer;
  if (ArrayBuffer.isView(message)) {
    return new Uint8Array(message.buffer, message.byteOffset, message.byteLength).slice().buffer;
  }
  return undefined;
}

function concat(a: Uint8Array, b: Uint8Array): Uint8Array {
  const joined = new Uint8Array(a.length + b.length);
  joined.set(a);
  joined.set(b, a.length);
  return joined;
}

/**
 * A gateway socket as a `TransportSocket`. Messages arrive on a channel, in order, `close` last.
 * Sends go one at a time, in order (the gateway's commands may otherwise run concurrently).
 * A frame the gateway refuses ends the socket with 1011, so the caller reconnects rather than
 * carry on with a hole in what it sent.
 */
class GatewaySocket implements TransportSocket {
  onopen: (() => void) | null = null;
  onmessage: ((message: { data: unknown }) => void) | null = null;
  onclose: ((close?: SocketClose) => void) | null = null;
  onerror: (() => void) | null = null;

  #id: number | undefined;
  #state: 'opening' | 'open' | 'closing' | 'closed' = 'opening';
  /** Frames not yet sent: text, or bytes. */
  readonly #outbox: Array<string | Uint8Array> = [];
  #sending = false;
  /** Set by `close()` until the close command goes out. */
  #closeWith: { code?: number | undefined; reason?: string | undefined } | undefined;
  /** Each warning once per socket. */
  readonly #warned = new Set<string>();

  constructor(workspace: string, path: string) {
    const events = new Channel<unknown>((message) => this.#receive(message));
    call<unknown>('gateway_socket_open', { workspace, path, events }).then(
      (answer) => {
        const id = openedId(answer);
        if (id === undefined) this.#failed(new GatewayError('internal', 'The gateway opened a socket without an id.'));
        else this.#opened(id);
      },
      (error: unknown) => this.#failed(toGatewayError(error)),
    );
  }

  send(data: string | ArrayBuffer | Uint8Array): void {
    if (this.#state === 'closing' || this.#state === 'closed') return;
    // A copy: the caller may reuse its buffer.
    this.#outbox.push(typeof data === 'string' ? data : data instanceof Uint8Array ? data.slice() : new Uint8Array(data.slice(0)));
    this.#flush();
  }

  close(code?: number, reason?: string): void {
    if (this.#state === 'closing' || this.#state === 'closed') return;
    // Closing before it opened sends nothing; once open, what was sent before goes first.
    if (this.#state === 'opening') this.#outbox.length = 0;
    this.#state = 'closing';
    this.#closeWith = { code, reason };
    this.#flush();
  }

  #opened(id: number): void {
    this.#id = id;
    if (this.#state === 'closed') return;
    if (this.#state === 'opening') {
      this.#state = 'open';
      this.onopen?.();
    }
    this.#flush();
  }

  /** Ends the socket here, with 1006: it never opened, or the gateway could not close it. */
  #failed(error: GatewayError): void {
    if (this.#state === 'closed') return;
    this.#state = 'closed';
    this.#outbox.length = 0;
    this.onerror?.();
    this.onclose?.({ code: 1006, reason: error.message, error });
  }

  #warn(message: string): void {
    if (this.#warned.has(message)) return;
    this.#warned.add(message);
    console.warn(`pitcrew: ${message}`);
  }

  #receive(message: unknown): void {
    if (this.#state === 'closed') return;
    const binary = asArrayBuffer(message);
    if (binary !== undefined) {
      if (this.#state !== 'closing') this.onmessage?.({ data: binary });
      return;
    }
    const frame = (typeof message === 'object' && message !== null ? message : {}) as {
      type?: unknown;
      data?: unknown;
      code?: unknown;
      reason?: unknown;
    };
    if (frame.type === 'text' && typeof frame.data === 'string') {
      if (this.#state !== 'closing') this.onmessage?.({ data: frame.data });
    } else if (frame.type === 'close') {
      this.#state = 'closed';
      this.#outbox.length = 0;
      this.onclose?.({
        code: typeof frame.code === 'number' ? frame.code : 1006,
        reason: typeof frame.reason === 'string' ? frame.reason : '',
      });
    } else {
      // A binary frame as a number array, say: the contract says ArrayBuffer.
      this.#warn('ignored a gateway socket message that is not text, an ArrayBuffer or close.');
    }
  }

  /** Sends the next frame, or the close once every frame before it has gone. */
  #flush(): void {
    const socket = this.#id;
    if (this.#sending || socket === undefined) return;
    if (this.#state !== 'open' && this.#state !== 'closing') return;
    const next = this.#outbox.shift();
    if (next === undefined) {
      const closeWith = this.#closeWith;
      if (this.#state !== 'closing' || closeWith === undefined) return;
      this.#closeWith = undefined;
      const args: Record<string, unknown> = { socket };
      if (closeWith.code !== undefined) args.code = closeWith.code;
      if (closeWith.reason !== undefined) args.reason = closeWith.reason;
      // The `close` message on the channel ends it. If the gateway cannot close it, end it here.
      call('gateway_socket_close', args).catch((error: unknown) => this.#failed(toGatewayError(error)));
      return;
    }
    let args: Record<string, unknown>;
    if (typeof next === 'string') {
      args = { socket, text: next };
    } else {
      // Bytes in a row are one stream (keystrokes): one frame carries them.
      let bytes = next;
      for (let more = this.#outbox[0]; more instanceof Uint8Array && bytes.length + more.length <= BATCH_BYTES; more = this.#outbox[0]) {
        bytes = concat(bytes, more);
        this.#outbox.shift();
      }
      args = { socket, binary: bytes };
    }
    this.#sending = true;
    const sent = () => {
      this.#sending = false;
      this.#flush();
    };
    const refused = (error: unknown) => {
      this.#sending = false;
      if (this.#state === 'closed') return;
      // The frame is lost, and so are the ones after it: end the socket, so the caller
      // reconnects (from where its data says) instead of carrying on with a hole.
      this.#warn(`a gateway socket send failed (${toGatewayError(error).message}); closing it with 1011.`);
      this.#outbox.length = 0;
      if (this.#state === 'open') {
        this.#state = 'closing';
        this.#closeWith = { code: 1011, reason: 'send failed' };
      }
      this.#flush();
    };
    call('gateway_socket_send', args).then(sent, refused);
  }
}
