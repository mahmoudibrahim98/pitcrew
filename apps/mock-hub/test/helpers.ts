// Shared test helpers: a fresh server per test, and a small JSON client.

import { startServer, type RunningServer, type ServerOptions } from '../src/server.ts';

export const DEVICE = 'dev-device-token';
export const AGENT = 'dev-agent-token';

/** Fixture ids used across the tests. */
export const ID = {
  sam: '01JB000000000000000MEM0001',
  writer: '01JB000000000000000MEM0002',
  runner: '01JB000000000000000MEM0003',
  paper: '01JB000000000000000PRJ0001',
  tooling: '01JB000000000000000PRJ0002',
  submission: '01JB000000000000000WST0001',
  parsers: '01JB000000000000000WST0003',
  pap1: '01JB000000000000000TSK0001',
  pap4: '01JB000000000000000TSK0004',
  ses1: '01JB000000000000000SES0001',
  ses2: '01JB000000000000000SES0002',
  ses3: '01JB000000000000000SES0003',
  ses4: '01JB000000000000000SES0004',
  ses5: '01JB000000000000000SES0005',
  ses6: '01JB000000000000000SES0006',
} as const;

/** Runs `test` against a fresh server and always closes it. */
export async function withServer(
  test: (server: RunningServer) => Promise<void>,
  options: ServerOptions = {},
): Promise<void> {
  const server = await startServer({ port: 0, ...options });
  try {
    await test(server);
  } finally {
    await server.close();
  }
}

export interface Reply<T> {
  status: number;
  headers: Headers;
  body: T;
}

export interface CallOptions {
  token?: string;
  /** Sent as JSON. */
  json?: unknown;
  /** Sent as is, for malformed bodies. */
  raw?: string;
  headers?: Record<string, string>;
}

/**
 * Calls the API. `T` is the wire type the test expects; the body is parsed JSON, cast at this
 * boundary, and `undefined` for empty replies.
 */
export async function call<T = unknown>(
  server: RunningServer,
  method: string,
  path: string,
  options: CallOptions = {},
): Promise<Reply<T>> {
  const headers: Record<string, string> = { ...options.headers };
  if (options.token !== undefined) {
    headers['authorization'] = `Bearer ${options.token}`;
  }
  let body: string | undefined = options.raw;
  if (options.json !== undefined) {
    headers['content-type'] = 'application/json';
    body = JSON.stringify(options.json);
  }
  const res = await fetch(server.url + path, { method, headers, body });
  const text = await res.text();
  return { status: res.status, headers: res.headers, body: (text === '' ? undefined : JSON.parse(text)) as T };
}

export function sleep(ms: number): Promise<void> {
  return new Promise((done) => setTimeout(done, ms));
}
