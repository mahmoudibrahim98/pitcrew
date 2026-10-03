// The `/v1/stream` connection: resume by `since`, reset when the hub's history is behind us,
// reconnect with back-off, and reconnect after 60 s without a frame. No React here. The socket
// comes from a transport, so the same client runs in a browser and through the desktop gateway.

import { socketUrl, type SocketClose, type SocketLike, type Transport } from './transport.ts';
import type { Event, StreamFrame } from './types.ts';

export { SUBPROTOCOL } from './transport.ts';
export type { SocketClose, SocketFactory, SocketLike } from './transport.ts';

export type StreamStatus = 'connecting' | 'live' | 'reconnecting' | 'stopped';

export interface StreamOptions {
  /** Where the socket comes from: the browser's WebSockets, or the desktop gateway. */
  transport: Transport;
  /** The revision the caller's data is at. Without it, the stream starts from the hub's `hello`. */
  since?: number | undefined;
  /** Events not seen before, in order. */
  onEvents(events: Event[]): void;
  /** The hub's history is behind `since` (it was reset): drop cached state and refetch. */
  onReset(rev: number): void;
  onStatus?(status: StreamStatus): void;
  /**
   * A connection failed; `attempts` counts failures since the last stable connection. `close`
   * says how, when the socket said (the desktop gateway says why it could not open one).
   */
  onFailure?(attempts: number, close?: SocketClose): void;
  /** Delay before reconnect attempt n is `min(maxMs, initialMs * 2^n)`, with jitter. */
  backoff?: { initialMs: number; maxMs: number };
  /** Reconnect when nothing arrives for this long. The hub pings every 20 s. */
  silenceMs?: number;
  /** The back-off starts over once a connection has stayed up this long. */
  stableMs?: number;
  random?: () => number;
}

/** The stream's path, resuming after `since` when given. */
export function streamPath(since: number | undefined): string {
  return since === undefined ? '/v1/stream' : `/v1/stream?since=${since}`;
}

/**
 * A terminal's socket path (API v1): `cols` and `rows` are 1..=1000; `from` is the number of
 * output bytes already received, when reconnecting.
 */
export function terminalPath(session: string, size: { cols: number; rows: number; from?: number }): string {
  const query = new URLSearchParams({ cols: String(size.cols), rows: String(size.rows) });
  if (size.from !== undefined) query.set('from', String(size.from));
  return `/v1/sessions/${encodeURIComponent(session)}/terminal?${query.toString()}`;
}

/** The browser's URL for the stream under an API base. */
export function streamUrl(baseUrl: string, since: number | undefined): string {
  return socketUrl(baseUrl, streamPath(since));
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
  #rev: number | undefined;
  /** The hub's event log (`hello.log`); revisions only compare within one log. */
  #log: string | undefined;
  #socket: SocketLike | undefined;
  #attempt = 0;
  #retryTimer: ReturnType<typeof setTimeout> | undefined;
  #silenceTimer: ReturnType<typeof setTimeout> | undefined;
  #stableTimer: ReturnType<typeof setTimeout> | undefined;
  #status: StreamStatus = 'stopped';
  /** `retryNow()` came while a connection was being opened: if it fails, retry without waiting. */
  #retrySoon = false;

  constructor(options: StreamOptions) {
    this.#options = options;
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
    this.#retrySoon = false;
    this.#detach()?.close();
    this.#setStatus('stopped');
  }

  /**
   * The hub is known to be back: reconnects now if waiting to, or, if a connection is being
   * opened, retries at once should that one fail.
   */
  retryNow(): void {
    if (this.#status === 'stopped' || this.#status === 'live') return;
    if (this.#retryTimer === undefined) {
      this.#retrySoon = true;
      return;
    }
    clearTimeout(this.#retryTimer);
    this.#connect();
  }

  #connect(): void {
    this.#retryTimer = undefined;
    const since = this.#rev;
    if (this.#status !== 'reconnecting') this.#setStatus('connecting');

    let socket: SocketLike;
    try {
      socket = this.#options.transport.openSocket(streamPath(since));
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
    socket.onclose = (close) => {
      if (socket !== this.#socket) return;
      this.#detach();
      this.#retry(close);
    };
    // A close always follows an error.
    socket.onerror = () => {};
    this.#armSilence();
  }

  #handle(frame: StreamFrame, since: number | undefined): void {
    switch (frame.type) {
      case 'hello': {
        this.#retrySoon = false;
        // A server that accepts and then drops us must not reset the back-off.
        this.#stableTimer = setTimeout(() => {
          this.#stableTimer = undefined;
          this.#attempt = 0;
        }, this.#options.stableMs ?? 5_000);
        // A different log is a different history (a restarted or replaced hub), even when its
        // revision has already passed ours. Hubs that predate `log` send none.
        const otherLog =
          frame.log !== undefined && this.#log !== undefined && frame.log !== this.#log;
        if (frame.log !== undefined) this.#log = frame.log;
        if (otherLog || (since !== undefined && since > frame.rev)) {
          this.#rev = frame.rev;
          this.#setStatus('live');
          this.#options.onReset(frame.rev);
          return;
        }
        // With `since`, the missed events follow as `events` frames and move `rev` on.
        if (since === undefined) this.#rev = frame.rev;
        this.#setStatus('live');
        return;
      }
      case 'events': {
        const rev = this.#rev ?? 0;
        if (frame.to_rev <= rev) return;
        // Private cursor writes create gaps between frames. Each frame itself is contiguous.
        // Skip any events already seen.
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

  #retry(close?: SocketClose): void {
    this.#clearTimers();
    this.#setStatus('reconnecting');
    const { initialMs, maxMs } = this.#options.backoff ?? { initialMs: 500, maxMs: 30_000 };
    const ceiling = Math.min(maxMs, initialMs * 2 ** this.#attempt);
    const jitter = 0.5 + (this.#options.random ?? Math.random)() * 0.5;
    const delay = this.#retrySoon ? 0 : ceiling * jitter;
    this.#retrySoon = false;
    this.#attempt += 1;
    this.#retryTimer = setTimeout(() => this.#connect(), delay);
    this.#options.onFailure?.(this.#attempt, close);
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
    if (this.#stableTimer !== undefined) clearTimeout(this.#stableTimer);
    this.#retryTimer = undefined;
    this.#silenceTimer = undefined;
    this.#stableTimer = undefined;
  }

  #setStatus(status: StreamStatus): void {
    if (status === this.#status) return;
    this.#status = status;
    this.#options.onStatus?.(status);
  }
}
