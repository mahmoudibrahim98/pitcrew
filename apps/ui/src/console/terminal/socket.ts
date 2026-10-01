// A session's terminal socket (`GET /v1/sessions/{id}/terminal`, api-v1.md "Terminals"), with no
// React. It counts the output bytes it has received and reconnects from there, so a dropped
// connection neither loses nor repeats output; holds at most 64 KiB of keystrokes while
// disconnected; and sends debounced, clamped resizes only when the size changed.
//
// It opens sockets through a factory that takes an API path, never a URL: the browser factory
// (`browserSocketFactory`) puts the path on the hub's base URL and the token in a subprotocol, and
// the desktop app's gateway will take the path as it is.

export const SUBPROTOCOL = 'pitcrew.v1';
const BEARER_PREFIX = 'pitcrew.bearer.';

/** The most keystroke bytes held while disconnected, and the most sent at once. */
export const INPUT_LIMIT = 64 * 1024;
/** Terminal sizes the hub accepts, in cells. */
export const MIN_SIZE = 1;
export const MAX_SIZE = 1000;

export interface TerminalSize {
  cols: number;
  rows: number;
}

/** How a socket closed. */
export interface CloseInfo {
  code: number;
  reason?: string | undefined;
  /** The HTTP status of a refused upgrade, when the transport knows it (a browser never does). */
  status?: number | undefined;
}

/** The part of a WebSocket the terminal uses; tests and the desktop gateway pass their own. */
export interface TerminalSocketLike {
  onopen: (() => void) | null;
  onmessage: ((message: { data: unknown }) => void) | null;
  onclose: ((event: CloseInfo) => void) | null;
  onerror: (() => void) | null;
  /** Bytes given to `send` and not yet on the wire. */
  readonly bufferedAmount?: number;
  send(data: string | Uint8Array): void;
  close(code?: number, reason?: string): void;
}

/** Opens a socket for an API path such as `/v1/sessions/{id}/terminal?cols=80&rows=24&from=0`. */
export type TerminalSocketFactory = (path: string) => TerminalSocketLike;

/** Why the hub will not serve this terminal; reconnecting cannot help. */
export interface TerminalProblem {
  /** The HTTP status it stands for (404, 503, …). */
  status: number;
  message: string;
}

export type WaitReason = 'hidden' | 'offline' | 'busy';

export type TerminalStatus =
  | { kind: 'connecting' }
  | { kind: 'live' }
  | { kind: 'reconnecting'; attempt: number; delayMs: number }
  /** Reconnecting waits: the page is hidden, the browser is offline, or the view is catching up. */
  | { kind: 'waiting'; why: WaitReason }
  /** The program ended (`exit`, or a 1000 close). Never reconnects. */
  | { kind: 'ended' }
  /** The hub will not serve it (a refused upgrade, or a close reconnecting cannot fix). */
  | { kind: 'stopped'; reason: string; code?: number; status?: number }
  /** Closed by its owner (`stop()`). */
  | { kind: 'closed' };

/** What became of keystrokes given to `send`. */
export type SendResult = 'sent' | 'queued' | 'refused' | 'closed';

/** When reconnecting should wait, and a way to hear that it may have changed. */
export interface Environment {
  blocked(): 'hidden' | 'offline' | undefined;
  subscribe(listener: () => void): () => void;
}

/** The page's visibility and the browser's network state. */
export const browserEnvironment: Environment = {
  blocked() {
    if (typeof navigator !== 'undefined' && navigator.onLine === false) return 'offline';
    if (typeof document !== 'undefined' && document.visibilityState === 'hidden') return 'hidden';
    return undefined;
  },
  subscribe(listener) {
    if (typeof window === 'undefined') return () => {};
    document.addEventListener('visibilitychange', listener);
    window.addEventListener('online', listener);
    window.addEventListener('offline', listener);
    return () => {
      document.removeEventListener('visibilitychange', listener);
      window.removeEventListener('online', listener);
      window.removeEventListener('offline', listener);
    };
  },
};

/** The API path of a terminal; the size is clamped as the hub requires. */
export function terminalPath(sessionId: string, size: TerminalSize, from: number): string {
  const query = new URLSearchParams({
    cols: String(clampSize(size.cols) ?? 80),
    rows: String(clampSize(size.rows) ?? 24),
    from: String(Math.max(0, Math.floor(from))),
  });
  return `/v1/sessions/${encodeURIComponent(sessionId)}/terminal?${query.toString()}`;
}

/** A whole number of cells within 1..=1000, or undefined for something that is not a number. */
export function clampSize(value: number): number | undefined {
  if (!Number.isFinite(value)) return undefined;
  return Math.min(MAX_SIZE, Math.max(MIN_SIZE, Math.round(value)));
}

export interface BrowserSocketOptions {
  /** The API base, for example `http://127.0.0.1:47317`; the scheme becomes `ws:` or `wss:`. */
  baseUrl: string;
  /** Sent only as the `pitcrew.bearer.<token>` subprotocol, never in the URL. */
  token?: string | undefined;
  /** What `new WebSocket(url, protocols)` does; tests pass their own. */
  create?: (url: string, protocols: string[]) => TerminalSocketLike;
}

function openWebSocket(url: string, protocols: string[]): TerminalSocketLike {
  const socket = new WebSocket(url, protocols);
  socket.binaryType = 'arraybuffer';
  return socket as unknown as TerminalSocketLike;
}

/** Opens browser WebSockets on the hub, relative to its base (so a path prefix is kept). */
export function browserSocketFactory(options: BrowserSocketOptions): TerminalSocketFactory {
  const base = options.baseUrl.endsWith('/') ? options.baseUrl : `${options.baseUrl}/`;
  const create = options.create ?? openWebSocket;
  return (path) => {
    const url = new URL(path.replace(/^\/+/, ''), base);
    url.protocol = url.protocol === 'https:' ? 'wss:' : 'ws:';
    const protocols = [SUBPROTOCOL];
    if (options.token !== undefined) protocols.push(BEARER_PREFIX + options.token);
    return create(url.toString(), protocols);
  };
}

/** Close codes after which reconnecting cannot help, and what they mean to a person. */
const FATAL_CLOSE: Readonly<Record<number, string>> = {
  1002: 'The hub reported a protocol error.',
  1003: 'The hub refused a message it cannot read.',
  1007: 'The hub refused a malformed control message.',
  1008: 'The hub refused the connection.',
  1009: 'A message was too large for the hub.',
  1011: 'The terminal failed on its machine.',
};

/** Statuses of a refused upgrade that reconnecting cannot fix. */
const FATAL_STATUS: Readonly<Record<number, string>> = {
  400: 'The hub refused the request for the terminal.',
  401: 'The hub refused this token, so the terminal cannot be shown.',
  403: 'The hub refused this token, so the terminal cannot be shown.',
  404: 'This session has no terminal.',
  503: 'Its machine cannot be reached right now, so the terminal cannot be shown.',
};

export interface TerminalSocketOptions {
  sessionId: string;
  socket: TerminalSocketFactory;
  /** The size to open with; `resize()` changes it. */
  size: TerminalSize;
  /** Output bytes, in order, each exactly once. */
  onOutput(bytes: Uint8Array): void;
  /** The hub no longer had the bytes asked for: the count jumped from `previous` to `from`. */
  onTruncated?(jump: { from: number; previous: number }): void;
  onStatus?(status: TerminalStatus): void;
  /**
   * Called when a connection failed before it opened. A browser cannot see the HTTP status of a
   * refused upgrade, so this finds out why (a 404 or a 503); undefined means the network failed,
   * and reconnecting may help.
   */
  diagnose?: () => Promise<TerminalProblem | undefined>;
  /** Delay before reconnect attempt n is `min(maxMs, initialMs * 2^n)`, times 0.5–1 (jitter). */
  backoff?: { initialMs: number; maxMs: number };
  /** The back-off starts over once a connection has stayed open this long. */
  stableMs?: number;
  /** Resizes wait this long for the size to settle. */
  resizeMs?: number;
  random?: () => number;
  environment?: Environment;
}

const encoder = new TextEncoder();

function asBytes(data: unknown): Uint8Array | undefined {
  if (data instanceof ArrayBuffer) return new Uint8Array(data);
  if (ArrayBuffer.isView(data)) return new Uint8Array(data.buffer, data.byteOffset, data.byteLength);
  return undefined;
}

function isOffset(value: unknown): value is number {
  return typeof value === 'number' && Number.isSafeInteger(value) && value >= 0;
}

export class TerminalSocket {
  readonly #options: TerminalSocketOptions;
  readonly #environment: Environment;
  #status: TerminalStatus = { kind: 'connecting' };
  #started = false;
  /** Ended, stopped or closed: nothing reconnects or sends any more. */
  #done = false;
  #socket: TerminalSocketLike | undefined;
  #open = false;
  #everOpened = false;
  /** Output bytes received: the offset of the next one, and the next connection's `from`. */
  #received = 0;
  #attempt = 0;
  /** Bumped whenever an outstanding diagnosis must be ignored. */
  #generation = 0;
  /** The view asked to stop reading until it catches up (`hold()`). */
  #held = false;
  #queue: Uint8Array[] = [];
  #queued = 0;
  /** The size wanted, and the size the hub was last told on this connection. */
  #size: TerminalSize;
  #sentSize: TerminalSize | undefined;
  #retryTimer: ReturnType<typeof setTimeout> | undefined;
  #stableTimer: ReturnType<typeof setTimeout> | undefined;
  #resizeTimer: ReturnType<typeof setTimeout> | undefined;
  #unsubscribe: (() => void) | undefined;

  constructor(options: TerminalSocketOptions) {
    this.#options = options;
    this.#environment = options.environment ?? browserEnvironment;
    this.#size = {
      cols: clampSize(options.size.cols) ?? 80,
      rows: clampSize(options.size.rows) ?? 24,
    };
  }

  get status(): TerminalStatus {
    return this.#status;
  }

  /** Output bytes received so far: where a reconnect resumes. */
  get received(): number {
    return this.#received;
  }

  /** Keystroke bytes waiting for a connection. */
  get queued(): number {
    return this.#queued;
  }

  start(): void {
    if (this.#started) return;
    this.#started = true;
    this.#unsubscribe = this.#environment.subscribe(() => this.#onEnvironment());
    this.#connect();
  }

  /** Closes for good, as the view goes away. */
  stop(): void {
    const wasDone = this.#done;
    this.#shutDown();
    if (!wasDone) this.#setStatus({ kind: 'closed' });
  }

  /**
   * Keystrokes (text is sent as UTF-8), as one binary message. While disconnected they wait, up to
   * 64 KiB in all; past that, or more than 64 KiB at once, they are refused.
   */
  send(data: string | Uint8Array): SendResult {
    if (this.#done) return 'closed';
    const bytes = typeof data === 'string' ? encoder.encode(data) : data.slice();
    if (bytes.byteLength === 0) return 'sent';
    if (bytes.byteLength > INPUT_LIMIT) return 'refused';
    const socket = this.#socket;
    if (socket !== undefined && this.#open && this.#queue.length === 0) {
      // A connection that is not draining what it was given is not handed more.
      if ((socket.bufferedAmount ?? 0) + bytes.byteLength > INPUT_LIMIT) return 'refused';
      socket.send(bytes);
      return 'sent';
    }
    if (this.#queued + bytes.byteLength > INPUT_LIMIT) return 'refused';
    this.#queue.push(bytes);
    this.#queued += bytes.byteLength;
    return 'queued';
  }

  /** The terminal's new size: clamped to 1..=1000, sent once it settles, and only if it changed. */
  resize(cols: number, rows: number): void {
    const c = clampSize(cols);
    const r = clampSize(rows);
    if (c === undefined || r === undefined || this.#done) return;
    this.#size = { cols: c, rows: r };
    if (this.#resizeTimer !== undefined) clearTimeout(this.#resizeTimer);
    this.#resizeTimer = setTimeout(() => {
      this.#resizeTimer = undefined;
      this.#sendSize();
    }, this.#options.resizeMs ?? 100);
  }

  /**
   * Stops reading until `resume()`: the connection closes, and the next one starts from what was
   * received. For a view that cannot keep up; the hub keeps the output meanwhile.
   */
  hold(): void {
    if (this.#held || this.#done) return;
    this.#held = true;
    this.#generation += 1;
    this.#clearRetry();
    this.#clearStable();
    this.#detach()?.close(1000, 'catching up');
    this.#setStatus({ kind: 'waiting', why: 'busy' });
  }

  resume(): void {
    if (!this.#held) return;
    this.#held = false;
    if (!this.#done && this.#socket === undefined && this.#started) this.#connect();
  }

  #connect(): void {
    this.#retryTimer = undefined;
    if (this.#done) return;
    const blocked = this.#held ? 'busy' : this.#environment.blocked();
    if (blocked !== undefined) {
      this.#setStatus({ kind: 'waiting', why: blocked });
      return;
    }
    const size = this.#size;
    let socket: TerminalSocketLike;
    try {
      socket = this.#options.socket(terminalPath(this.#options.sessionId, size, this.#received));
    } catch {
      this.#finish({ kind: 'stopped', reason: 'Could not open a connection to the terminal.' });
      return;
    }
    this.#socket = socket;
    this.#open = false;
    this.#sentSize = size;
    if (this.#status.kind !== 'reconnecting') {
      this.#setStatus(this.#everOpened ? { kind: 'reconnecting', attempt: this.#attempt, delayMs: 0 } : { kind: 'connecting' });
    }
    socket.onopen = () => {
      if (socket !== this.#socket) return;
      this.#open = true;
      this.#everOpened = true;
      this.#clearStable();
      this.#stableTimer = setTimeout(() => {
        this.#stableTimer = undefined;
        this.#attempt = 0;
      }, this.#options.stableMs ?? 5_000);
      this.#setStatus({ kind: 'live' });
      this.#flush(socket);
      if (this.#resizeTimer === undefined) this.#sendSize();
    };
    socket.onmessage = (message) => {
      if (socket === this.#socket) this.#receive(message.data);
    };
    socket.onclose = (event) => {
      if (socket !== this.#socket) return;
      const wasOpen = this.#open;
      this.#detach();
      this.#closed(event, wasOpen);
    };
    // A close always follows an error.
    socket.onerror = () => {};
  }

  #receive(data: unknown): void {
    if (typeof data === 'string') {
      this.#control(data);
      return;
    }
    const bytes = asBytes(data);
    if (bytes === undefined || bytes.byteLength === 0) return;
    this.#received += bytes.byteLength;
    this.#options.onOutput(bytes);
  }

  #control(text: string): void {
    let frame: unknown;
    try {
      frame = JSON.parse(text);
    } catch {
      return;
    }
    if (typeof frame !== 'object' || frame === null) return;
    const { type, from } = frame as { type?: unknown; from?: unknown };
    if (type === 'truncated' && isOffset(from)) {
      const previous = this.#received;
      this.#received = from;
      this.#options.onTruncated?.({ from, previous });
    } else if (type === 'exit') {
      this.#finish({ kind: 'ended' });
    }
    // Other types are for newer clients.
  }

  #closed(event: CloseInfo, wasOpen: boolean): void {
    if (this.#done) return;
    this.#clearStable();
    if (event.code === 1000) {
      this.#finish({ kind: 'ended' });
      return;
    }
    const fatal = FATAL_CLOSE[event.code];
    if (fatal !== undefined) {
      const reason = event.reason === undefined || event.reason === '' ? fatal : `${fatal} (${event.reason})`;
      this.#finish({ kind: 'stopped', reason, code: event.code });
      return;
    }
    if (!wasOpen) {
      // The upgrade failed. A transport that knows the HTTP status says so; a browser does not.
      const known = event.status === undefined ? undefined : FATAL_STATUS[event.status];
      if (known !== undefined && event.status !== undefined) {
        this.#finish({ kind: 'stopped', reason: known, code: event.code, status: event.status });
        return;
      }
      if (event.status === undefined && this.#options.diagnose !== undefined) {
        this.#diagnose(this.#options.diagnose);
        return;
      }
    }
    // 1001, 1013, 1006 and anything else: the connection may come back.
    this.#retry();
  }

  #diagnose(diagnose: () => Promise<TerminalProblem | undefined>): void {
    const generation = ++this.#generation;
    const settle = (problem: TerminalProblem | undefined) => {
      if (generation !== this.#generation || this.#done || this.#held) return;
      if (problem === undefined) this.#retry();
      else this.#finish({ kind: 'stopped', reason: problem.message, status: problem.status });
    };
    diagnose().then(settle, () => settle(undefined));
  }

  #retry(): void {
    this.#clearRetry();
    if (this.#done || this.#held) return;
    const { initialMs, maxMs } = this.#options.backoff ?? { initialMs: 500, maxMs: 15_000 };
    const ceiling = Math.min(maxMs, initialMs * 2 ** this.#attempt);
    const delayMs = Math.round(ceiling * (0.5 + (this.#options.random ?? Math.random)() * 0.5));
    this.#attempt += 1;
    this.#setStatus({ kind: 'reconnecting', attempt: this.#attempt, delayMs });
    this.#retryTimer = setTimeout(() => this.#connect(), delayMs);
  }

  /** The page came back or went online: a reconnect that was waiting for that goes now. */
  #onEnvironment(): void {
    if (this.#done || this.#status.kind !== 'waiting' || this.#status.why === 'busy') return;
    const blocked = this.#environment.blocked();
    if (blocked === undefined) this.#connect();
    else if (blocked !== this.#status.why) this.#setStatus({ kind: 'waiting', why: blocked });
  }

  #flush(socket: TerminalSocketLike): void {
    const queue = this.#queue;
    this.#queue = [];
    this.#queued = 0;
    for (const bytes of queue) socket.send(bytes);
  }

  #sendSize(): void {
    const socket = this.#socket;
    if (socket === undefined || !this.#open) return;
    const size = this.#size;
    if (this.#sentSize !== undefined && this.#sentSize.cols === size.cols && this.#sentSize.rows === size.rows) return;
    socket.send(JSON.stringify({ type: 'resize', cols: size.cols, rows: size.rows }));
    this.#sentSize = size;
  }

  #finish(status: TerminalStatus): void {
    this.#shutDown();
    this.#setStatus(status);
  }

  #shutDown(): void {
    this.#done = true;
    this.#generation += 1;
    this.#clearRetry();
    this.#clearStable();
    if (this.#resizeTimer !== undefined) clearTimeout(this.#resizeTimer);
    this.#resizeTimer = undefined;
    this.#unsubscribe?.();
    this.#unsubscribe = undefined;
    this.#queue = [];
    this.#queued = 0;
    this.#detach()?.close(1000, 'closed by the viewer');
  }

  /** Forgets the current socket so its late callbacks are ignored, and returns it. */
  #detach(): TerminalSocketLike | undefined {
    const socket = this.#socket;
    this.#socket = undefined;
    this.#open = false;
    if (socket !== undefined) {
      socket.onopen = null;
      socket.onmessage = null;
      socket.onclose = null;
      socket.onerror = null;
    }
    return socket;
  }

  #clearRetry(): void {
    if (this.#retryTimer !== undefined) clearTimeout(this.#retryTimer);
    this.#retryTimer = undefined;
  }

  #clearStable(): void {
    if (this.#stableTimer !== undefined) clearTimeout(this.#stableTimer);
    this.#stableTimer = undefined;
  }

  #setStatus(status: TerminalStatus): void {
    const current = this.#status;
    if (
      current.kind === status.kind &&
      (status.kind === 'connecting' || status.kind === 'live' || status.kind === 'ended' || status.kind === 'closed')
    ) {
      return;
    }
    this.#status = status;
    this.#options.onStatus?.(status);
  }
}
