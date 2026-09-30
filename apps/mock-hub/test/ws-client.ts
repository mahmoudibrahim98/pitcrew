// A tiny WebSocket client for the tests, written separately from src/ws.ts so the two check each
// other. It masks every frame it sends, as RFC 6455 requires of clients, unless a test asks it not
// to.

import { createHash, randomBytes } from 'node:crypto';
import { request, type IncomingMessage } from 'node:http';
import type { Duplex } from 'node:stream';

const GUID = '258EAFA5-E914-47DA-95CA-C5AB0DC85B11';

export type Message =
  | { type: 'text'; text: string }
  | { type: 'binary'; data: Buffer }
  | { type: 'pong'; data: Buffer }
  | { type: 'close'; code: number; reason: string };

/** The server refused the handshake with an HTTP error. */
export class Refused extends Error {
  readonly status: number;
  readonly body: unknown;

  constructor(status: number, body: unknown) {
    super(`handshake refused with ${status}`);
    this.status = status;
    this.body = body;
  }
}

export class TestSocket {
  /** The subprotocol the server selected. */
  readonly protocol: string | undefined;

  readonly #socket: Duplex;
  #buffer: Buffer = Buffer.alloc(0);
  readonly #queue: Message[] = [];
  readonly #waiters: ((message: Message) => void)[] = [];
  #closed = false;
  #sentClose = false;

  private constructor(socket: Duplex, head: Buffer, protocol: string | undefined) {
    this.#socket = socket;
    this.protocol = protocol;
    socket.on('data', (chunk: Buffer) => this.#receive(chunk));
    socket.on('close', () => this.#push({ type: 'close', code: 1006, reason: 'socket closed' }));
    socket.on('error', () => socket.destroy());
    if (head.length > 0) {
      this.#receive(head);
    }
  }

  /** Opens a WebSocket at `url` (an http:// URL) offering `protocols`. */
  static connect(url: string, protocols: string[]): Promise<TestSocket> {
    const key = randomBytes(16).toString('base64');
    const headers: Record<string, string> = {
      Connection: 'Upgrade',
      Upgrade: 'websocket',
      'Sec-WebSocket-Key': key,
      'Sec-WebSocket-Version': '13',
    };
    if (protocols.length > 0) {
      headers['Sec-WebSocket-Protocol'] = protocols.join(', ');
    }
    return new Promise((resolve, reject) => {
      const req = request(url, { headers });
      req.on('upgrade', (res: IncomingMessage, socket: Duplex, head: Buffer) => {
        const expected = createHash('sha1').update(key + GUID).digest('base64');
        if (res.headers['sec-websocket-accept'] !== expected) {
          socket.destroy();
          reject(new Error('wrong Sec-WebSocket-Accept'));
          return;
        }
        const protocol = res.headers['sec-websocket-protocol'];
        resolve(new TestSocket(socket, head, Array.isArray(protocol) ? protocol[0] : protocol));
      });
      req.on('response', (res: IncomingMessage) => {
        const chunks: Buffer[] = [];
        res.on('data', (chunk: Buffer) => chunks.push(chunk));
        res.on('end', () => {
          const text = Buffer.concat(chunks).toString('utf8');
          reject(new Refused(res.statusCode ?? 0, text === '' ? undefined : JSON.parse(text)));
        });
      });
      req.on('error', reject);
      req.end();
    });
  }

  sendText(text: string): void {
    this.sendFrame(0x1, Buffer.from(text, 'utf8'));
  }

  sendBinary(data: Buffer): void {
    this.sendFrame(0x2, data);
  }

  sendFrame(opcode: number, payload: Buffer, options: { fin?: boolean; mask?: boolean } = {}): void {
    this.#socket.write(encodeFrame(opcode, payload, options.fin ?? true, options.mask ?? true));
  }

  /** Writes bytes as they are, for frames the encoder refuses to make. */
  sendRaw(bytes: Buffer): void {
    this.#socket.write(bytes);
  }

  close(code = 1000): void {
    if (this.#sentClose) {
      return;
    }
    this.#sentClose = true;
    const payload = Buffer.alloc(2);
    payload.writeUInt16BE(code, 0);
    this.sendFrame(0x8, payload);
  }

  /** The next message, or a rejection after `timeoutMs`. */
  next(timeoutMs = 3000): Promise<Message> {
    const queued = this.#queue.shift();
    if (queued !== undefined) {
      return Promise.resolve(queued);
    }
    return new Promise((resolve, reject) => {
      const timer = setTimeout(() => {
        const index = this.#waiters.indexOf(waiter);
        if (index !== -1) {
          this.#waiters.splice(index, 1);
        }
        reject(new Error(`no message within ${timeoutMs} ms`));
      }, timeoutMs);
      const waiter = (message: Message): void => {
        clearTimeout(timer);
        resolve(message);
      };
      this.#waiters.push(waiter);
    });
  }

  /** The next message, which must be a text frame holding JSON. */
  async nextJson<T = unknown>(timeoutMs?: number): Promise<T> {
    const message = await this.next(timeoutMs);
    if (message.type !== 'text') {
      throw new Error(`expected a text frame, got ${message.type}`);
    }
    return JSON.parse(message.text) as T;
  }

  /** Resolves if nothing arrives for `ms`; rejects with the message if something does. */
  async expectQuiet(ms: number): Promise<void> {
    const message = await this.next(ms).catch(() => undefined);
    if (message !== undefined) {
      throw new Error(`expected silence, got ${JSON.stringify(message)}`);
    }
  }

  destroy(): void {
    this.#socket.destroy();
  }

  #push(message: Message): void {
    if (this.#closed) {
      return;
    }
    if (message.type === 'close') {
      this.#closed = true;
    }
    const waiter = this.#waiters.shift();
    if (waiter !== undefined) {
      waiter(message);
    } else {
      this.#queue.push(message);
    }
  }

  #receive(chunk: Buffer): void {
    this.#buffer = Buffer.concat([this.#buffer, chunk]);
    for (;;) {
      const frame = decodeFrame(this.#buffer);
      if (frame === undefined) {
        return;
      }
      this.#buffer = this.#buffer.subarray(frame.size);
      this.#handle(frame.opcode, frame.payload);
    }
  }

  #handle(opcode: number, payload: Buffer): void {
    switch (opcode) {
      case 0x1:
        this.#push({ type: 'text', text: payload.toString('utf8') });
        break;
      case 0x2:
        this.#push({ type: 'binary', data: payload });
        break;
      case 0x8: {
        const code = payload.length >= 2 ? payload.readUInt16BE(0) : 1005;
        // Answer the server's close, as RFC 6455 asks of both ends.
        this.close(code === 1005 ? 1000 : code);
        this.#push({ type: 'close', code, reason: payload.subarray(2).toString('utf8') });
        break;
      }
      case 0x9:
        this.sendFrame(0xa, payload);
        break;
      case 0xa:
        this.#push({ type: 'pong', data: payload });
        break;
      default:
        throw new Error(`unexpected opcode ${opcode} from the server`);
    }
  }
}

/** A client frame: masked unless told otherwise. */
export function encodeFrame(opcode: number, payload: Buffer, fin = true, mask = true): Buffer {
  const length = payload.length;
  const lengthBytes = length < 126 ? 0 : length < 0x10000 ? 2 : 8;
  const head = Buffer.alloc(2 + lengthBytes);
  head.writeUInt8((fin ? 0x80 : 0) | opcode, 0);
  const code = lengthBytes === 0 ? length : lengthBytes === 2 ? 126 : 127;
  head.writeUInt8((mask ? 0x80 : 0) | code, 1);
  if (lengthBytes === 2) {
    head.writeUInt16BE(length, 2);
  } else if (lengthBytes === 8) {
    head.writeBigUInt64BE(BigInt(length), 2);
  }
  if (!mask) {
    return Buffer.concat([head, payload]);
  }
  const key = randomBytes(4);
  const masked = Buffer.alloc(length);
  for (let i = 0; i < length; i++) {
    masked.writeUInt8(payload.readUInt8(i) ^ key.readUInt8(i % 4), i);
  }
  return Buffer.concat([head, key, masked]);
}

/** One server frame from the start of `buf`, or `undefined` if it is not all there yet. */
function decodeFrame(buf: Buffer): { opcode: number; payload: Buffer; size: number } | undefined {
  if (buf.length < 2) {
    return undefined;
  }
  const b0 = buf.readUInt8(0);
  const b1 = buf.readUInt8(1);
  if ((b1 & 0x80) !== 0) {
    throw new Error('server frames must not be masked');
  }
  if ((b0 & 0x80) === 0) {
    throw new Error('the mock never fragments; this client does not reassemble');
  }
  let length = b1 & 0x7f;
  let offset = 2;
  if (length === 126) {
    if (buf.length < 4) {
      return undefined;
    }
    length = buf.readUInt16BE(2);
    offset = 4;
  } else if (length === 127) {
    if (buf.length < 10) {
      return undefined;
    }
    length = Number(buf.readBigUInt64BE(2));
    offset = 10;
  }
  if (buf.length < offset + length) {
    return undefined;
  }
  return {
    opcode: b0 & 0x0f,
    payload: Buffer.from(buf.subarray(offset, offset + length)),
    size: offset + length,
  };
}
