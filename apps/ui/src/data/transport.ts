// How requests and sockets reach a daemon: one seam, two transports, chosen once at start.
//
// - In a browser (development only): `fetch` and `WebSocket` to the API's URL, with the bearer
//   token as a header and as the `pitcrew.bearer.` subprotocol.
// - In the desktop app: the gateway's commands and channels (`gateway.ts`, loaded only there).
//   The gateway adds the token; the webview never holds one (ADR-0003).

import { ApiError } from './errors.ts';

export type Method = 'GET' | 'POST' | 'PATCH' | 'PUT' | 'DELETE';

/** A daemon's answer, whatever its status. */
export interface TransportResponse {
  status: number;
  contentType?: string | undefined;
  /** The body as text (API v1 bodies are JSON). */
  body: string;
  /** HTTP's reason phrase, when the transport has one (the browser's does; the gateway's not). */
  statusText?: string | undefined;
}

/** How a socket ended. `error` says why it never opened, when the transport knows. */
export interface SocketClose {
  code: number;
  reason: string;
  error?: ApiError | undefined;
}

/** The part of the browser `WebSocket` the stream uses; tests pass a fake. */
export interface SocketLike {
  /** Text frames as strings, binary frames as `ArrayBuffer`s. */
  onmessage: ((message: { data: unknown }) => void) | null;
  /** Called once, last. */
  onclose: ((close?: SocketClose) => void) | null;
  onerror: (() => void) | null;
  close(code?: number, reason?: string): void;
}

/** Makes a browser socket: `new WebSocket(url, protocols)` unless a test passes its own. */
export type SocketFactory = (url: string, protocols: string[]) => SocketLike;

/** A socket from a transport: the same in a browser and in the desktop app. */
export interface TransportSocket extends SocketLike {
  onopen: (() => void) | null;
  /**
   * A string is a text frame, bytes a binary frame. Frames sent before the socket opens wait for
   * it, in order; once `close()` is called or the socket has closed, they are dropped.
   */
  send(data: string | ArrayBuffer | Uint8Array): void;
  /**
   * Bytes sent but not yet taken by the connection: the browser `WebSocket`'s own
   * `bufferedAmount`, or, in the desktop app, the bytes still queued in the gateway transport's
   * outbox. A sender that keeps writing without watching this is outrunning the connection.
   */
  readonly bufferedAmount: number;
}

export interface Transport {
  readonly kind: 'browser' | 'desktop';
  /** Names the far end in messages: the API's URL, or the workspace. */
  readonly label: string;
  /**
   * One API v1 request. `path` starts with `/v1/` and may carry a query; `body` is JSON text.
   * Resolves with every answer the daemon gives, whatever its status. Rejects with an `ApiError`
   * (status 0) when there was no answer, or with the `AbortError` of `signal`.
   */
  request(method: Method, path: string, body?: string, signal?: AbortSignal): Promise<TransportResponse>;
  /**
   * Opens one of API v1's WebSocket routes (`/v1/stream`, `/v1/sessions/{id}/terminal`), with its
   * query. A plain function: it may be passed around unbound.
   */
  openSocket(path: string): TransportSocket;
  /**
   * Hands an integration's secret to the daemon (`PUT /v1/integrations/{id}/credential`). In the
   * desktop app this is the gateway's own command (`gateway_integration_credential`), never
   * `request`, which refuses that route. Absent where `request` may carry it (a browser).
   */
  storeCredential?(integration: string, secret: string): Promise<TransportResponse>;
}

/** Inside the desktop app's webview (`docs/build/contracts/desktop-gateway.md`). */
export function isDesktop(): boolean {
  return typeof window !== 'undefined' && '__TAURI_INTERNALS__' in window;
}

// ─── The browser transport ──────────────────────────────────────────────────────────────────────

export const SUBPROTOCOL = 'pitcrew.v1';
const BEARER_PREFIX = 'pitcrew.bearer.';

export interface BrowserOptions {
  /** For example `http://127.0.0.1:47317`, without a trailing slash. */
  baseUrl: string;
  /** Sent as `Authorization: Bearer` and as a subprotocol. Development only. */
  token?: string | undefined;
  fetch?: typeof fetch;
  socket?: SocketFactory;
}

export interface BrowserTransport extends Transport {
  readonly kind: 'browser';
  readonly baseUrl: string;
  /** The same API with another token or socket factory (tests acting as someone else). */
  with(changes: Pick<BrowserOptions, 'token' | 'socket'>): BrowserTransport;
}

/** A socket route's URL under the API base, keeping a path prefix (`https://host/hub/`). */
export function socketUrl(baseUrl: string, path: string): string {
  const url = new URL(path.replace(/^\/+/, ''), baseUrl.endsWith('/') ? baseUrl : `${baseUrl}/`);
  url.protocol = url.protocol === 'https:' ? 'wss:' : 'ws:';
  return url.toString();
}

const webSocket: SocketFactory = (url, protocols) => new WebSocket(url, protocols) as unknown as SocketLike;

export function browserTransport(options: BrowserOptions): BrowserTransport {
  const baseUrl = options.baseUrl.replace(/\/+$/, '');
  const { token } = options;
  const doFetch = options.fetch ?? globalThis.fetch.bind(globalThis);
  const makeSocket = options.socket ?? webSocket;

  async function request(
    method: Method,
    path: string,
    body?: string,
    signal?: AbortSignal,
  ): Promise<TransportResponse> {
    const headers: Record<string, string> = { Accept: 'application/json' };
    if (token !== undefined) headers.Authorization = `Bearer ${token}`;
    if (body !== undefined) headers['Content-Type'] = 'application/json';
    try {
      const res = await doFetch(`${baseUrl}${path}`, { method, headers, body: body ?? null, signal: signal ?? null });
      return {
        status: res.status,
        statusText: res.statusText === '' ? undefined : res.statusText,
        contentType: res.headers.get('content-type') ?? undefined,
        body: await res.text(),
      };
    } catch (cause) {
      if (cause instanceof DOMException && cause.name === 'AbortError') throw cause;
      throw new ApiError('unavailable', `Cannot reach ${baseUrl}`, 0);
    }
  }

  function openSocket(path: string): TransportSocket {
    const protocols = [SUBPROTOCOL];
    if (token !== undefined) protocols.push(BEARER_PREFIX + token);
    return wrapBrowserSocket(makeSocket(socketUrl(baseUrl, path), protocols));
  }

  return {
    kind: 'browser',
    label: baseUrl,
    baseUrl,
    request,
    openSocket,
    with: (changes) => browserTransport({ ...options, ...changes }),
  };
}

interface RawSocket extends SocketLike {
  onopen?: (() => void) | null;
  readyState?: number;
  binaryType?: string;
  /** A real `WebSocket` always has this; a test's fake may not. */
  bufferedAmount?: number;
  send?(data: string | ArrayBuffer | Uint8Array): void;
}

/** Gives a `WebSocket` (or a test's fake) the transport's socket behaviour. */
function wrapBrowserSocket(raw: RawSocket): TransportSocket {
  if ('binaryType' in raw) raw.binaryType = 'arraybuffer';
  const OPEN = 1;
  let state: 'opening' | 'open' | 'closed' = raw.readyState === OPEN ? 'open' : 'opening';
  const waiting: Array<string | ArrayBuffer | Uint8Array> = [];

  const socket: TransportSocket = {
    onopen: null,
    onmessage: null,
    onclose: null,
    onerror: null,
    get bufferedAmount() {
      return raw.bufferedAmount ?? 0;
    },
    send(data) {
      if (state === 'opening') waiting.push(data);
      else if (state === 'open') raw.send?.(data);
    },
    close(code, reason) {
      state = 'closed';
      waiting.length = 0;
      if (code === undefined) raw.close();
      else raw.close(code, reason);
    },
  };
  raw.onopen = () => {
    if (state !== 'opening') return;
    state = 'open';
    for (const data of waiting.splice(0)) raw.send?.(data);
    socket.onopen?.();
  };
  raw.onmessage = (message) => socket.onmessage?.(message);
  raw.onerror = () => socket.onerror?.();
  raw.onclose = (close) => {
    state = 'closed';
    socket.onclose?.({ code: close?.code ?? 1006, reason: close?.reason ?? '' });
  };
  return socket;
}
