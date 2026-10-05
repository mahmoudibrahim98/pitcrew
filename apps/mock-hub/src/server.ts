// The mock hub: PitCrew's API v1 over HTTP and WebSocket, served from the demo workspace.
//
// Run `node apps/mock-hub/src/server.ts` (the port comes from PORT, default 47317). It listens on
// 127.0.0.1 only. Tests call `startServer({ port: 0 })`; each call gets a fresh copy of the demo
// workspace, so servers never share state.

import { createServer, type IncomingMessage, type Server, type ServerResponse } from 'node:http';
import type { AddressInfo } from 'node:net';
import { resolve } from 'node:path';
import type { Duplex } from 'node:stream';
import { fileURLToPath } from 'node:url';
import {
  openStream,
  openTerminal,
  streamSince,
  terminalTarget,
  type TerminalTarget,
} from './live.ts';
import * as integrations from './integrations.ts';
import { openSignInTerminal, signInTerminal } from './machine-setup.ts';
import { loadRecaps } from './recaps.ts';
import { MOCK_VERSION, authenticate, handleApi, type Reply } from './routes.ts';
import {
  DEFAULT_DELAYS,
  DEFAULT_SCAN_WINDOW,
  Hub,
  freshWorkspace,
  loadFixture,
  type Delays,
} from './state.ts';
import type { MemberId } from './types.ts';
import { ApiFailure, forbidden, invalid, notFound } from './validate.ts';
import {
  acceptUpgrade,
  handshakeProblem,
  offeredProtocols,
  rejectUpgrade,
  type WebSocketConnection,
} from './ws.ts';

/** The demo workspace, resolved from this file so the server runs from any directory. */
const FIXTURE = new URL('../../../crates/fixtures/data/demo-workspace.json', import.meta.url);
/** The demo workspace's recaps, as the recap engine writes them. */
const RECAPS = new URL('../../../crates/fixtures/data/demo-recaps.json', import.meta.url);

export const DEFAULT_PORT = 47317;
const HOST = '127.0.0.1';
const MAX_BODY_BYTES = 1024 * 1024;
const SUBPROTOCOL = 'pitcrew.v1';
const BEARER_SUBPROTOCOL = 'pitcrew.bearer.';

export interface ServerOptions {
  /** 0 picks a free port. Default 47317. */
  port?: number;
  /** Shorter delays make tests of simulated sessions quick. */
  delays?: Partial<Delays>;
  /** How many revisions one filtered `GET /v1/events` examines at most. Default 500. */
  scanWindow?: number;
  /**
   * Serves an empty workspace (no members, machines or work) instead of the demo fixture, so
   * `POST /v1/setup` can be exercised. `PITCREW_MOCK_FRESH=1` sets this from the command line.
   */
  fresh?: boolean;
  /** Receives one line per request. Silent by default. */
  log?: (line: string) => void;
  /**
   * The folder of recorded GitHub and Jira exchanges integrations read (`*.fixture`), instead of
   * `fixtures/`. It is read again at each sync, so a test can change what "upstream" says.
   */
  integrationFixtures?: string;
}

export interface RunningServer {
  /** For example `http://127.0.0.1:47317`. */
  url: string;
  port: number;
  /** This run's event-log id, as sent in the stream's `hello`. */
  logId: string;
  /** The in-memory state, for tests that need events no route makes (such as `brief_proposed`). */
  hub: Hub;
  close(): Promise<void>;
}

/** Starts a mock hub on 127.0.0.1. */
export async function startServer(options: ServerOptions = {}): Promise<RunningServer> {
  const hub = new Hub(
    options.fresh === true ? freshWorkspace() : loadFixture(FIXTURE),
    { ...DEFAULT_DELAYS, ...options.delays },
    options.scanWindow ?? DEFAULT_SCAN_WINDOW,
    options.fresh === true ? undefined : loadRecaps(RECAPS),
  );
  if (options.integrationFixtures !== undefined) integrations.useFixtures(hub, options.integrationFixtures);
  const log = options.log ?? ((): void => {});
  const sockets = new Set<WebSocketConnection>();
  const server = createServer((req, res) => {
    // Tells a developer which server answered when another program shares the port.
    res.setHeader('X-PitCrew-Mock-Hub', MOCK_VERSION);
    void serveHttp(hub, req, res, log);
  });
  server.on('upgrade', (req: IncomingMessage, socket: Duplex, head: Buffer) => {
    const conn = serveUpgrade(hub, req, socket, head, log);
    if (conn !== undefined) {
      sockets.add(conn);
      void conn.closed.then(() => sockets.delete(conn));
    }
  });
  await listen(server, options.port ?? DEFAULT_PORT);
  const { port } = server.address() as AddressInfo;
  return {
    url: `http://${HOST}:${port}`,
    port,
    logId: hub.logId,
    hub,
    close: () => shutdown(server, hub, sockets),
  };
}

function listen(server: Server, port: number): Promise<void> {
  return new Promise((done, fail) => {
    server.once('error', fail);
    server.listen(port, HOST, () => {
      server.off('error', fail);
      done();
    });
  });
}

async function shutdown(server: Server, hub: Hub, sockets: Set<WebSocketConnection>): Promise<void> {
  hub.dispose();
  const closing = [...sockets].map((conn) => {
    conn.close(1001, 'server shutting down');
    return conn.closed;
  });
  await Promise.race([Promise.all(closing), sleep(500)]);
  for (const conn of sockets) {
    conn.terminate();
  }
  await new Promise<void>((done) => {
    server.close(() => done());
    server.closeAllConnections();
  });
}

// ─── HTTP ───────────────────────────────────────────────────────────────────────────────────────

async function serveHttp(
  hub: Hub,
  req: IncomingMessage,
  res: ServerResponse,
  log: (line: string) => void,
): Promise<void> {
  const started = Date.now();
  const method = req.method ?? 'GET';
  const { path, query } = splitUrl(req.url ?? '/');
  const fileRoute = /^\/v1\/workstreams\/[^/]+\/files(?:\/content)?$/.test(path);
  if (fileRoute) {
    res.setHeader('Cache-Control', 'no-store');
    res.setHeader('X-Content-Type-Options', 'nosniff');
  }
  let reply: Reply;
  try {
    checkHost(req);
    const origin = req.headers.origin;
    res.setHeader('Vary', 'Origin');
    if (origin !== undefined && isAllowedOrigin(origin)) {
      setCorsHeaders(res, origin);
    }
    reply =
      method === 'OPTIONS'
        ? preflight(origin)
        : await handleApi(hub, {
            method,
            path,
            query,
            authorization: req.headers.authorization,
            readBody: () => readJson(req, fileRoute),
          });
  } catch (error) {
    reply = failureReply(error);
  }
  send(res, reply);
  log(`${method} ${path} ${reply.status} ${Date.now() - started}ms`);
}

/** Browsers ask before sending `Authorization`; only local origins are told yes. */
function preflight(origin: string | undefined): Reply {
  if (origin !== undefined && !isAllowedOrigin(origin)) {
    throw forbidden(
      `Origin ${origin} is not allowed; use http://localhost:PORT or http://127.0.0.1:PORT.`,
    );
  }
  return { status: 204 };
}

function setCorsHeaders(res: ServerResponse, origin: string): void {
  res.setHeader('Access-Control-Allow-Origin', origin);
  res.setHeader('Access-Control-Allow-Methods', 'GET, POST, PUT, PATCH, OPTIONS');
  res.setHeader('Access-Control-Allow-Headers', 'Authorization, Content-Type');
  res.setHeader('Access-Control-Max-Age', '600');
}

/** Origins of the Tauri app: macOS and Linux use the custom scheme, Windows `tauri.localhost`. */
const TAURI_ORIGINS = new Set(['tauri://localhost', 'http://tauri.localhost', 'https://tauri.localhost']);

/** `http://localhost:*`, `http://127.0.0.1:*` and the Tauri app's origins. */
export function isAllowedOrigin(origin: string): boolean {
  return TAURI_ORIGINS.has(origin) || /^http:\/\/(localhost|127\.0\.0\.1)(:\d{1,5})?$/.test(origin);
}

/**
 * Refuses requests addressed to other host names. Only loopback names reach 127.0.0.1 honestly;
 * anything else is a page using DNS rebinding to borrow the well-known dev tokens.
 */
function checkHost(req: IncomingMessage): void {
  const host = req.headers.host;
  if (host === undefined) {
    return;
  }
  const name = host.replace(/:\d+$/, '').toLowerCase();
  const loopback =
    name === 'localhost' || name === '127.0.0.1' || name === '[::1]' || name.endsWith('.localhost');
  if (!loopback) {
    throw forbidden(`This mock only answers requests for localhost, not ${host}.`);
  }
}

function splitUrl(url: string): { path: string; query: URLSearchParams } {
  const mark = url.indexOf('?');
  return mark === -1
    ? { path: url, query: new URLSearchParams() }
    : { path: url.slice(0, mark), query: new URLSearchParams(url.slice(mark + 1)) };
}

/** The request body as JSON; `undefined` when empty. Bodies over 1 MiB are refused. */
async function readJson(req: IncomingMessage, files = false): Promise<unknown> {
  const bytes = await readBody(req, files);
  if (bytes.length === 0) {
    return undefined;
  }
  try {
    return JSON.parse(bytes.toString('utf8')) as unknown;
  } catch {
    throw invalid('The body is not valid JSON.');
  }
}

/**
 * Reads the whole body, keeping at most MAX_BODY_BYTES. A larger body is still read to its end
 * (and dropped), so the client gets a clean 400 instead of a reset connection.
 */
function readBody(req: IncomingMessage, files = false): Promise<Buffer> {
  const cap = files ? 12 * 1024 * 1024 : MAX_BODY_BYTES;
  return new Promise((done, fail) => {
    const chunks: Buffer[] = [];
    let size = 0;
    req.on('data', (chunk: Buffer) => {
      size += chunk.length;
      if (size <= cap) {
        chunks.push(chunk);
      }
    });
    req.on('end', () => {
      if (size > cap) {
        fail(files ? new ApiFailure('too_large', 'File body exceeds the size cap.') : invalid(`The body is larger than ${MAX_BODY_BYTES} bytes.`));
      } else {
        done(Buffer.concat(chunks));
      }
    });
    req.on('error', fail);
  });
}

function failureReply(error: unknown): Reply {
  if (error instanceof ApiFailure) {
    return { status: error.status, body: error.toBody() };
  }
  console.error('mock-hub: unexpected error:', error);
  const failure = new ApiFailure('internal', 'The mock hub failed; see its console.');
  return { status: failure.status, body: failure.toBody() };
}

function send(res: ServerResponse, reply: Reply): void {
  if (reply.status === 401) {
    res.setHeader('WWW-Authenticate', 'Bearer');
  }
  if (reply.stream !== undefined) {
    res.writeHead(reply.status, {
      'Content-Type': reply.stream.contentType,
      'Cache-Control': 'no-store',
      'X-Content-Type-Options': 'nosniff',
    });
    // A client that went away misses the rest; the route's work goes on to its end regardless.
    const open = (): boolean => !res.destroyed && !res.writableEnded;
    reply.stream.start({
      canceled: () => !open(),
      write: (line) => {
        if (open()) res.write(line);
      },
      end: () => {
        if (open()) res.end();
      },
    });
    return;
  }
  if (reply.body === undefined) {
    res.writeHead(reply.status).end();
    return;
  }
  const text = JSON.stringify(reply.body);
  res
    .writeHead(reply.status, {
      'Content-Type': 'application/json; charset=utf-8',
      'Content-Length': Buffer.byteLength(text),
      'Cache-Control': 'no-store',
      'X-Content-Type-Options': 'nosniff',
    })
    .end(text);
}

// ─── WebSockets ─────────────────────────────────────────────────────────────────────────────────

/**
 * Checks and accepts a WebSocket upgrade for the stream or a terminal. The token comes as the
 * subprotocol `pitcrew.bearer.<token>`, and the server answers with `pitcrew.v1`. Both sockets
 * need a device token. Returns the connection, or `undefined` if the request was refused.
 */
function serveUpgrade(
  hub: Hub,
  req: IncomingMessage,
  socket: Duplex,
  head: Buffer,
  log: (line: string) => void,
): WebSocketConnection | undefined {
  socket.on('error', () => socket.destroy());
  const { path, query } = splitUrl(req.url ?? '/');
  try {
    checkHost(req);
    const origin = req.headers.origin;
    if (origin !== undefined && !isAllowedOrigin(origin)) {
      throw forbidden(`Origin ${origin} is not allowed.`);
    }
    const problem = handshakeProblem(req);
    if (problem !== undefined) {
      throw invalid(problem);
    }
    const target = socketRoute(path);
    const protocols = offeredProtocols(req);
    if (!protocols.includes(SUBPROTOCOL)) {
      throw invalid(`Offer the subprotocol ${SUBPROTOCOL}.`);
    }
    const token = protocols
      .find((p) => p.startsWith(BEARER_SUBPROTOCOL))
      ?.slice(BEARER_SUBPROTOCOL.length);
    if (token === undefined) {
      throw new ApiFailure(
        'unauthorized',
        `No token: offer it as a subprotocol, "${BEARER_SUBPROTOCOL}" followed by the token.`,
      );
    }
    const caller = authenticate(hub, token);
    if (caller.scope !== 'device') {
      throw forbidden(`${path} needs a device token.`);
    }
    const conn =
      target.kind === 'stream'
        ? acceptStream(hub, req, socket, head, streamSince(query), caller.memberId)
        : acceptTerminalOrSignIn(hub, req, socket, head, target.session, query, caller.memberId);
    log(`WS ${path} 101`);
    return conn;
  } catch (error) {
    const reply = failureReply(error);
    rejectUpgrade(socket, reply.status, reply.body);
    log(`WS ${path} ${reply.status}`);
    return undefined;
  }
}

function acceptStream(
  hub: Hub,
  req: IncomingMessage,
  socket: Duplex,
  head: Buffer,
  since: number | undefined,
  person: string,
): WebSocketConnection {
  const conn = acceptUpgrade(req, socket, head, SUBPROTOCOL);
  openStream(hub, conn, since, person);
  return conn;
}

function acceptTerminal(
  hub: Hub,
  req: IncomingMessage,
  socket: Duplex,
  head: Buffer,
  target: TerminalTarget,
): WebSocketConnection {
  const conn = acceptUpgrade(req, socket, head, SUBPROTOCOL);
  openTerminal(hub, conn, target);
  return conn;
}

/** A sign-in's terminal (machine-setup.ts), else a session's. */
function acceptTerminalOrSignIn(
  hub: Hub,
  req: IncomingMessage,
  socket: Duplex,
  head: Buffer,
  ref: string,
  query: URLSearchParams,
  member: MemberId,
): WebSocketConnection {
  const signIn = signInTerminal(hub, ref, member);
  if (signIn === undefined) {
    return acceptTerminal(hub, req, socket, head, terminalTarget(hub, ref, query));
  }
  const conn = acceptUpgrade(req, socket, head, SUBPROTOCOL);
  openSignInTerminal(hub, conn, signIn);
  return conn;
}

function socketRoute(path: string): { kind: 'stream' } | { kind: 'terminal'; session: string } {
  if (path === '/v1/stream') {
    return { kind: 'stream' };
  }
  const match = /^\/v1\/sessions\/([^/]+)\/terminal$/.exec(path);
  if (match?.[1] === undefined) {
    throw notFound(`No WebSocket at ${path}.`);
  }
  try {
    return { kind: 'terminal', session: decodeURIComponent(match[1]) };
  } catch {
    throw invalid(`Malformed path segment "${match[1]}".`);
  }
}

// ─── Command line ───────────────────────────────────────────────────────────────────────────────

function sleep(ms: number): Promise<void> {
  return new Promise((done) => setTimeout(done, ms).unref());
}

function parsePort(value: string | undefined): number {
  if (value === undefined || value === '') {
    return DEFAULT_PORT;
  }
  const port = /^\d{1,5}$/.test(value) ? Number(value) : Number.NaN;
  if (!(port <= 65535)) {
    throw new Error(`PORT must be a number from 0 to 65535, not "${value}".`);
  }
  return port;
}

function parseScanWindow(value: string | undefined): number {
  if (value === undefined || value === '') {
    return DEFAULT_SCAN_WINDOW;
  }
  const window = /^\d{1,9}$/.test(value) ? Number(value) : Number.NaN;
  if (!(window >= 1)) {
    throw new Error(`PITCREW_MOCK_SCAN_WINDOW must be a whole number of at least 1, not "${value}".`);
  }
  return window;
}

/** `PITCREW_MOCK_FRESH=1` serves an empty workspace instead of the demo. */
function parseFresh(value: string | undefined): boolean {
  return value === '1';
}

async function main(): Promise<void> {
  const fresh = parseFresh(process.env['PITCREW_MOCK_FRESH']);
  const server = await startServer({
    port: parsePort(process.env['PORT']),
    scanWindow: parseScanWindow(process.env['PITCREW_MOCK_SCAN_WINDOW']),
    fresh,
    log: (line) => console.log(line),
  });
  console.log(`PitCrew mock hub on ${server.url}`);
  if (fresh) {
    console.log('  fresh mode: no workspace yet; POST /v1/setup with the device token to create one.');
    console.log('  device token: dev-device-token  (a member nothing knows until setup)');
  } else {
    console.log('  device token: dev-device-token  (@sam)');
    console.log('  agent token:  dev-agent-token   (@writer)');
  }
  const stop = (): void => {
    void server.close().then(() => process.exit(0));
  };
  process.once('SIGINT', stop);
  process.once('SIGTERM', stop);
}

function isEntryPoint(): boolean {
  const entry = process.argv[1];
  if (entry === undefined) {
    return false;
  }
  const self = fileURLToPath(import.meta.url);
  const same = (a: string, b: string): boolean =>
    process.platform === 'win32' ? a.toLowerCase() === b.toLowerCase() : a === b;
  return same(resolve(entry), self);
}

if (isEntryPoint()) {
  main().catch((error: unknown) => {
    const code = (error as { code?: unknown } | null)?.code;
    console.error(
      code === 'EADDRINUSE'
        ? `mock-hub: port ${process.env['PORT'] ?? DEFAULT_PORT} is in use; set PORT to another one.`
        : `mock-hub: ${error instanceof Error ? error.message : String(error)}`,
    );
    process.exit(1);
  });
}
