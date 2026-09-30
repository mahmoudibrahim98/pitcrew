// The `/v1/stream` connection: resume by `since`, reset when the hub's history is behind us,
// reconnect with back-off, and reconnect after 60 s without a frame. No React here.

import type { Event, StreamFrame } from './types.ts';

export type StreamStatus = 'connecting' | 'live' | 'reconnecting' | 'stopped';

/** The part of the browser `WebSocket` the client uses; tests pass a fake. */
export interface SocketLike {
  onmessage: ((message: { data: unknown }) => void) | null;
  onclose: (() => void) | null;
  onerror: (() => void) | null;
  close(): void;
}
export type SocketFactory = (url: string, protocols: string[]) => SocketLike;

export interface StreamOptions {
  /** The API base, for example `http://127.0.0.1:47317`; the scheme becomes `ws:` or `wss:`. */
  baseUrl: string;
  /** Sent as the `pitcrew.bearer.<token>` subprotocol. Absent in the desktop app. */
  token?: string | undefined;
  /** The revision the caller's data is at. Without it, the stream starts from the hub's `hello`. */
  since?: number | undefined;
  /** Events not seen before, in order. */
  onEvents(events: Event[]): void;
  /** The hub's history is behind `since` (it was reset): drop cached state and refetch. */
  onReset(rev: number): void;
  onStatus?(status: StreamStatus): void;
  socket?: SocketFactory;
  /** Delay before reconnect attempt n is `min(maxMs, initialMs * 2^n)`, with jitter. */
  backoff?: { initialMs: number; maxMs: number };
  /** Reconnect when nothing arrives for this long. The hub pings every 20 s. */
  silenceMs?: number;
  random?: () => number;
}

export const SUBPROTOCOL = 'pitcrew.v1';
const BEARER_PREFIX = 'pitcrew.bearer.';

const browserSocket: SocketFactory = (url, protocols) =>
  new WebSocket(url, protocols) as unknown as SocketLike;

export function streamUrl(baseUrl: string, since: number | undefined): string {
  const url = new URL('/v1/stream', baseUrl);
  url.protocol = url.protocol === 'https:' ? 'wss:' : 'ws:';
  if (since !== undefined) url.searchParams.set('since', String(since));
  return url.toString();
}

function parseFrame(data: unknown): StreamFrame | undefined {
  if (typeof data !== 'string') return undefined;
  try {
    const frame = JSON.parse(data) as { type?: unknown };
    return typeof frame === 'object' && frame !== null && typeof frame.type === 'string'
      ? (frame as StreamFrame)
      : undefined;
  } catch {
    return undefined;
  }
}

export class StreamClient {
  readonly #options: StreamOptions;
  readonly #socketFactory: SocketFactory;
  #rev: number | undefined;
  #socket: SocketLike | undefined;
  #attempt = 0;
  #retryTimer: ReturnType<typeof setTimeout> | undefined;
  #silenceTimer: ReturnType<typeof setTimeout> | undefined;
  #status: StreamStatus = 'stopped';

  constructor(options: StreamOptions) {
    this.#options = options;
    this.#socketFactory = options.socket ?? browserSocket;
    this.#rev = options.since;
  }

  /** The last revision received; the next connection resumes from here. */
  get rev(): number | undefined {
    return this.#rev;
  }

  get status(): StreamStatus {
    return this.#status;
  }

  start(): void {
    if (this.#status !== 'stopped') return;
    this.#connect();
  }

  stop(): void {
    this.#clearTimers();
    this.#detach()?.close();
    this.#setStatus('stopped');
  }

  #connect(): void {
    this.#retryTimer = undefined;
    const since = this.#rev;
    const protocols = [SUBPROTOCOL];
    if (this.#options.token !== undefined) protocols.push(BEARER_PREFIX + this.#options.token);
    if (this.#status !== 'reconnecting') this.#setStatus('connecting');

    let socket: SocketLike;
    try {
      socket = this.#socketFactory(streamUrl(this.#options.baseUrl, since), protocols);
    } catch {
      this.#retry();
      return;
    }
    this.#socket = socket;
    socket.onmessage = (message) => {
      if (socket !== this.#socket) return;
      this.#armSilence();
      const frame = parseFrame(message.data);
      if (frame !== undefined) this.#handle(frame, since);
    };
    socket.onclose = () => {
      if (socket !== this.#socket) return;
      this.#detach();
      this.#retry();
    };
    // A close always follows an error.
    socket.onerror = () => {};
    this.#armSilence();
  }

  #handle(frame: StreamFrame, since: number | undefined): void {
    switch (frame.type) {
      case 'hello':
        this.#attempt = 0;
        if (since !== undefined && since > frame.rev) {
          this.#rev = frame.rev;
          this.#setStatus('live');
          this.#options.onReset(frame.rev);
          return;
        }
        // With `since`, the missed events follow as `events` frames and move `rev` on.
        if (since === undefined) this.#rev = frame.rev;
        this.#setStatus('live');
        return;
      case 'events': {
        const rev = this.#rev ?? 0;
        if (frame.to_rev <= rev) return;
        // Frames are contiguous; skip any events already seen.
        const fresh =
          frame.from_rev <= rev ? frame.events.slice(rev - frame.from_rev + 1) : frame.events;
        this.#rev = frame.to_rev;
        if (fresh.length > 0) this.#options.onEvents(fresh);
        return;
      }
      case 'ping':
        return;
    }
  }

  #armSilence(): void {
    if (this.#silenceTimer !== undefined) clearTimeout(this.#silenceTimer);
    this.#silenceTimer = setTimeout(() => {
      this.#silenceTimer = undefined;
      this.#detach()?.close();
      this.#retry();
    }, this.#options.silenceMs ?? 60_000);
  }

  #retry(): void {
    this.#clearTimers();
    this.#setStatus('reconnecting');
    const { initialMs, maxMs } = this.#options.backoff ?? { initialMs: 500, maxMs: 30_000 };
    const ceiling = Math.min(maxMs, initialMs * 2 ** this.#attempt);
    const jitter = 0.5 + (this.#options.random ?? Math.random)() * 0.5;
    this.#attempt += 1;
    this.#retryTimer = setTimeout(() => this.#connect(), ceiling * jitter);
  }

  /** Forgets the current socket so its late callbacks are ignored, and returns it. */
  #detach(): SocketLike | undefined {
    const socket = this.#socket;
    this.#socket = undefined;
    if (socket !== undefined) {
      socket.onmessage = null;
      socket.onclose = null;
      socket.onerror = null;
    }
    return socket;
  }

  #clearTimers(): void {
    if (this.#retryTimer !== undefined) clearTimeout(this.#retryTimer);
    if (this.#silenceTimer !== undefined) clearTimeout(this.#silenceTimer);
    this.#retryTimer = undefined;
    this.#silenceTimer = undefined;
  }

  #setStatus(status: StreamStatus): void {
    if (status === this.#status) return;
    this.#status = status;
    this.#options.onStatus?.(status);
  }
}
