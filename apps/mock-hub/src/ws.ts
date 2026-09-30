// A minimal WebSocket server (RFC 6455), enough for the mock hub's two sockets.
//
// - The handshake answers with `Sec-WebSocket-Accept` and, when asked, one subprotocol.
// - Text, binary, ping, pong and close frames; fragmented messages are put back together.
// - Payload lengths use the 7-bit, 16-bit and 64-bit forms. Frames and whole messages are capped
//   (1 MiB by default); anything larger closes the connection with 1009.
// - Client frames must be masked; an unmasked one closes the connection with 1002.
// - No extensions: permessage-deflate is never negotiated. Outgoing frames are never fragmented.

import { createHash } from 'node:crypto';
import { STATUS_CODES, type IncomingMessage } from 'node:http';
import type { Duplex } from 'node:stream';

export const MAX_PAYLOAD = 1024 * 1024;

const GUID = '258EAFA5-E914-47DA-95CA-C5AB0DC85B11';
const CLOSE_TIMEOUT_MS = 1000;

const OP_CONTINUATION = 0x0;
const OP_TEXT = 0x1;
const OP_BINARY = 0x2;
const OP_CLOSE = 0x8;
const OP_PING = 0x9;
const OP_PONG = 0xa;
const KNOWN_OPCODES = new Set([OP_CONTINUATION, OP_TEXT, OP_BINARY, OP_CLOSE, OP_PING, OP_PONG]);

const UTF8 = new TextDecoder('utf-8', { fatal: true });

/** How a connection ended: the close code sent or received, or 1006 if it just dropped. */
export interface CloseInfo {
  code: number;
  reason: string;
}

// ─── Handshake ──────────────────────────────────────────────────────────────────────────────────

/** Why `req` is not a valid WebSocket opening handshake, or `undefined` if it is. */
export function handshakeProblem(req: IncomingMessage): string | undefined {
  if (req.method !== 'GET') {
    return 'A WebSocket handshake must use GET.';
  }
  if (header(req, 'upgrade')?.toLowerCase() !== 'websocket') {
    return 'Expected "Upgrade: websocket".';
  }
  const connection = (header(req, 'connection') ?? '').toLowerCase().split(',');
  if (!connection.some((token) => token.trim() === 'upgrade')) {
    return 'Expected "Connection: Upgrade".';
  }
  if (header(req, 'sec-websocket-version') !== '13') {
    return 'Only WebSocket version 13 is supported.';
  }
  if (!/^[A-Za-z0-9+/]{22}==$/.test(header(req, 'sec-websocket-key') ?? '')) {
    return 'Sec-WebSocket-Key must be 16 bytes in base64.';
  }
  return undefined;
}

/** The subprotocols the client offered, in its order. */
export function offeredProtocols(req: IncomingMessage): string[] {
  const raw = req.headers['sec-websocket-protocol'];
  const joined = Array.isArray(raw) ? raw.join(',') : (raw ?? '');
  return joined
    .split(',')
    .map((p) => p.trim())
    .filter((p) => p !== '');
}

/** The `Sec-WebSocket-Accept` value for a client key. */
export function acceptKey(key: string): string {
  return createHash('sha1').update(key + GUID).digest('base64');
}

/** Answers an upgrade request with an HTTP error and a JSON body, then closes the socket. */
export function rejectUpgrade(socket: Duplex, status: number, body: unknown): void {
  const text = JSON.stringify(body);
  const head = [
    `HTTP/1.1 ${status} ${STATUS_CODES[status] ?? ''}`,
    'Content-Type: application/json; charset=utf-8',
    `Content-Length: ${Buffer.byteLength(text)}`,
    'Connection: close',
  ];
  socket.end(`${head.join('\r\n')}\r\n\r\n${text}`);
}

/**
 * Completes the handshake of a request that passed `handshakeProblem`, selecting `protocol` if
 * given (it must be one the client offered).
 */
export function acceptUpgrade(
  req: IncomingMessage,
  socket: Duplex,
  head: Buffer,
  protocol?: string,
  maxPayload = MAX_PAYLOAD,
): WebSocketConnection {
  const key = header(req, 'sec-websocket-key');
  if (key === undefined) {
    throw new Error('acceptUpgrade needs a request that passed handshakeProblem');
  }
  const lines = [
    'HTTP/1.1 101 Switching Protocols',
    'Upgrade: websocket',
    'Connection: Upgrade',
    `Sec-WebSocket-Accept: ${acceptKey(key)}`,
  ];
  if (protocol !== undefined) {
    lines.push(`Sec-WebSocket-Protocol: ${protocol}`);
  }
  socket.write(`${lines.join('\r\n')}\r\n\r\n`);
  return new WebSocketConnection(socket, head, maxPayload);
}

function header(req: IncomingMessage, name: string): string | undefined {
  const value = req.headers[name];
  return Array.isArray(value) ? value[0] : value;
}

// ─── Frames ─────────────────────────────────────────────────────────────────────────────────────

interface Frame {
  fin: boolean;
  opcode: number;
  payload: Buffer;
}

type Parsed =
  | { kind: 'frame'; frame: Frame; size: number }
  | { kind: 'incomplete' }
  | { kind: 'error'; code: number; reason: string };

const INCOMPLETE: Parsed = { kind: 'incomplete' };

const failure = (code: number, reason: string): Parsed => ({ kind: 'error', code, reason });

/**
 * Parses one client frame from the start of `buf`. Size limits are checked as soon as the
 * header is in, so an oversized frame is refused before its payload arrives.
 */
function parseFrame(buf: Buffer, maxPayload: number): Parsed {
  if (buf.length < 2) {
    return INCOMPLETE;
  }
  const b0 = buf.readUInt8(0);
  const b1 = buf.readUInt8(1);
  const fin = (b0 & 0x80) !== 0;
  const opcode = b0 & 0x0f;
  if ((b0 & 0x70) !== 0) {
    return failure(1002, 'reserved bits are set');
  }
  if (!KNOWN_OPCODES.has(opcode)) {
    return failure(1002, `unknown opcode ${opcode}`);
  }
  if ((b1 & 0x80) === 0) {
    return failure(1002, 'client frames must be masked');
  }
  let length = b1 & 0x7f;
  if (opcode >= OP_CLOSE && (!fin || length > 125)) {
    return failure(1002, 'control frames must be final and at most 125 bytes');
  }
  let offset = 2;
  if (length === 126) {
    if (buf.length < 4) {
      return INCOMPLETE;
    }
    length = buf.readUInt16BE(2);
    offset = 4;
  } else if (length === 127) {
    if (buf.length < 10) {
      return INCOMPLETE;
    }
    const long = buf.readBigUInt64BE(2);
    if (long > BigInt(maxPayload)) {
      return failure(1009, 'frame too big');
    }
    length = Number(long);
    offset = 10;
  }
  if (length > maxPayload) {
    return failure(1009, 'frame too big');
  }
  const size = offset + 4 + length;
  if (buf.length < size) {
    return INCOMPLETE;
  }
  const mask = buf.subarray(offset, offset + 4);
  const payload = Buffer.from(buf.subarray(offset + 4, size));
  for (let i = 0; i < payload.length; i++) {
    payload.writeUInt8(payload.readUInt8(i) ^ mask.readUInt8(i & 3), i);
  }
  return { kind: 'frame', frame: { fin, opcode, payload }, size };
}

/** One unmasked, final server frame. */
function encodeFrame(opcode: number, payload: Buffer): Buffer {
  const length = payload.length;
  let head: Buffer;
  if (length < 126) {
    head = Buffer.alloc(2);
    head.writeUInt8(length, 1);
  } else if (length < 0x10000) {
    head = Buffer.alloc(4);
    head.writeUInt8(126, 1);
    head.writeUInt16BE(length, 2);
  } else {
    head = Buffer.alloc(10);
    head.writeUInt8(127, 1);
    head.writeBigUInt64BE(BigInt(length), 2);
  }
  head.writeUInt8(0x80 | opcode, 0);
  return Buffer.concat([head, payload]);
}

function closePayload(code: number, reason: string): Buffer {
  const text = Buffer.from(reason, 'utf8').subarray(0, 123);
  const payload = Buffer.alloc(2 + text.length);
  payload.writeUInt16BE(code, 0);
  text.copy(payload, 2);
  return payload;
}

/** A close frame's code and reason; 1005 when it has none; `undefined` if it is malformed. */
function parseClose(payload: Buffer): CloseInfo | undefined {
  if (payload.length === 0) {
    return { code: 1005, reason: '' };
  }
  if (payload.length === 1) {
    return undefined;
  }
  const code = payload.readUInt16BE(0);
  const valid =
    (code >= 1000 && code <= 1014 && code !== 1004 && code !== 1005 && code !== 1006) ||
    (code >= 3000 && code <= 4999);
  if (!valid) {
    return undefined;
  }
  try {
    return { code, reason: UTF8.decode(payload.subarray(2)) };
  } catch {
    return undefined;
  }
}

// ─── Connection ─────────────────────────────────────────────────────────────────────────────────

type ReadyState = 'open' | 'closing' | 'closed';

/** One accepted WebSocket. Set `onText` and `onBinary`; `closed` settles once, when it ends. */
export class WebSocketConnection {
  onText: (text: string) => void = () => {};
  onBinary: (data: Buffer) => void = () => {};
  readonly closed: Promise<CloseInfo>;

  readonly #socket: Duplex;
  readonly #maxPayload: number;
  #resolveClosed: (info: CloseInfo) => void = () => {};
  #state: ReadyState = 'open';
  #closeInfo: CloseInfo = { code: 1006, reason: '' };
  #buffer: Buffer = Buffer.alloc(0);
  #fragmentOpcode = 0;
  #fragments: Buffer[] = [];
  #fragmentSize = 0;
  #closeTimer: ReturnType<typeof setTimeout> | undefined;

  constructor(socket: Duplex, head: Buffer, maxPayload: number) {
    this.#socket = socket;
    this.#maxPayload = maxPayload;
    this.closed = new Promise((resolve) => {
      this.#resolveClosed = resolve;
    });
    socket.on('data', (chunk: Buffer) => this.#receive(chunk));
    socket.on('end', () => socket.end());
    socket.on('error', () => socket.destroy());
    socket.on('close', () => this.#finish());
    if (head.length > 0) {
      // Bytes that came with the handshake; handled after the caller has set its handlers.
      queueMicrotask(() => this.#receive(head));
    }
  }

  get isOpen(): boolean {
    return this.#state === 'open';
  }

  sendText(text: string): void {
    this.#send(OP_TEXT, Buffer.from(text, 'utf8'));
  }

  sendBinary(data: Uint8Array): void {
    this.#send(OP_BINARY, Buffer.from(data.buffer, data.byteOffset, data.byteLength));
  }

  /** Starts the closing handshake; the socket is dropped if the peer does not answer in time. */
  close(code = 1000, reason = ''): void {
    if (this.#state !== 'open') {
      return;
    }
    this.#closeInfo = { code, reason };
    this.#write(OP_CLOSE, closePayload(code, reason));
    this.#state = 'closing';
    this.#dropLater();
  }

  /** Drops the connection without a closing handshake. */
  terminate(): void {
    this.#socket.destroy();
  }

  #send(opcode: number, payload: Buffer): void {
    if (this.#state === 'open') {
      this.#write(opcode, payload);
    }
  }

  #write(opcode: number, payload: Buffer): void {
    if (this.#socket.writable) {
      this.#socket.write(encodeFrame(opcode, payload));
    }
  }

  #receive(chunk: Buffer): void {
    this.#buffer = this.#buffer.length === 0 ? chunk : Buffer.concat([this.#buffer, chunk]);
    while (this.#state !== 'closed') {
      const parsed = parseFrame(this.#buffer, this.#maxPayload);
      if (parsed.kind === 'incomplete') {
        return;
      }
      if (parsed.kind === 'error') {
        this.#fail(parsed.code, parsed.reason);
        return;
      }
      this.#buffer = this.#buffer.subarray(parsed.size);
      this.#handle(parsed.frame);
    }
  }

  #handle(frame: Frame): void {
    switch (frame.opcode) {
      case OP_CLOSE:
        this.#handleClose(frame.payload);
        return;
      case OP_PING:
        this.#send(OP_PONG, frame.payload);
        return;
      case OP_PONG:
        return;
      case OP_CONTINUATION:
        this.#continueMessage(frame);
        return;
      default:
        this.#startMessage(frame);
    }
  }

  #startMessage(frame: Frame): void {
    if (this.#fragmentOpcode !== 0) {
      this.#fail(1002, 'expected a continuation frame');
    } else if (frame.fin) {
      this.#deliver(frame.opcode, frame.payload);
    } else {
      this.#fragmentOpcode = frame.opcode;
      this.#fragments = [frame.payload];
      this.#fragmentSize = frame.payload.length;
    }
  }

  #continueMessage(frame: Frame): void {
    if (this.#fragmentOpcode === 0) {
      this.#fail(1002, 'continuation frame without a message');
      return;
    }
    this.#fragmentSize += frame.payload.length;
    if (this.#fragmentSize > this.#maxPayload) {
      this.#fail(1009, 'message too big');
      return;
    }
    this.#fragments.push(frame.payload);
    if (frame.fin) {
      const opcode = this.#fragmentOpcode;
      const payload = Buffer.concat(this.#fragments);
      this.#fragmentOpcode = 0;
      this.#fragments = [];
      this.#fragmentSize = 0;
      this.#deliver(opcode, payload);
    }
  }

  #deliver(opcode: number, payload: Buffer): void {
    if (this.#state !== 'open') {
      return;
    }
    if (opcode === OP_BINARY) {
      this.onBinary(payload);
      return;
    }
    let text: string;
    try {
      text = UTF8.decode(payload);
    } catch {
      this.#fail(1007, 'text frames must be valid UTF-8');
      return;
    }
    this.onText(text);
  }

  #handleClose(payload: Buffer): void {
    const info = parseClose(payload);
    if (info === undefined) {
      this.#fail(1002, 'malformed close frame');
      return;
    }
    if (this.#state === 'open') {
      // Answer with the same code, or an empty close frame if the peer sent none.
      this.#closeInfo = info;
      this.#write(OP_CLOSE, info.code === 1005 ? Buffer.alloc(0) : closePayload(info.code, ''));
    }
    this.#state = 'closed';
    this.#socket.end();
    this.#dropLater();
  }

  /** Closes with `code` after a protocol error, without waiting for the peer. */
  #fail(code: number, reason: string): void {
    if (this.#state === 'open') {
      this.#closeInfo = { code, reason };
      this.#write(OP_CLOSE, closePayload(code, reason));
    }
    this.#state = 'closed';
    this.#socket.end();
    this.#dropLater();
  }

  #dropLater(): void {
    clearTimeout(this.#closeTimer);
    this.#closeTimer = setTimeout(() => this.#socket.destroy(), CLOSE_TIMEOUT_MS);
  }

  #finish(): void {
    clearTimeout(this.#closeTimer);
    this.#state = 'closed';
    this.#resolveClosed(this.#closeInfo);
  }
}
