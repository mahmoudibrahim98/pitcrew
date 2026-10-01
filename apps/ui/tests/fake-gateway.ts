// A fake desktop gateway: Tauri's IPC mocked with `mockIPC`, answering the commands, the event
// and the channels of docs/build/contracts/desktop-gateway.md. Each workspace's daemon is a
// function from a request to a response; sockets are driven by the test. Every call is recorded,
// so a test can check what the webview sent. Needs a DOM (happy-dom) for `window`.

import { emit } from '@tauri-apps/api/event';
import { clearMocks, mockIPC } from '@tauri-apps/api/mocks';
import type { GatewayErrorCode } from '../src/data/errors.ts';
import type { GatewayRequest, GatewayResponse } from '../src/data/gateway.ts';
import type { Member, Project, Workspace } from '../src/data/types.ts';
import type { GatewayWorkspace } from '../src/data/workspaces.tsx';

export type Daemon = (req: GatewayRequest) => GatewayResponse | Promise<GatewayResponse>;

interface Internals {
  runCallback(id: number, data: unknown): void;
}

function internals(): Internals {
  return (window as unknown as { __TAURI_INTERNALS__: Internals }).__TAURI_INTERNALS__;
}

/** A rejected command, as the gateway rejects it. */
function failure(code: GatewayErrorCode, message: string): Promise<never> {
  return Promise.reject({ code, message });
}

/** The contract's path checks, for requests. */
function badPath(path: string): boolean {
  const [route = ''] = path.split('?');
  return (
    !path.startsWith('/v1/') ||
    route.split('/').includes('..') ||
    path.includes('//') ||
    path.includes('\\') ||
    path.includes('#') ||
    // eslint-disable-next-line no-control-regex
    /[\u0000-\u001f\u007f]/.test(path)
  );
}

const SOCKET_ROUTE = /^\/v1\/(stream|sessions\/[^/?#]+\/terminal)(\?.*)?$/;

export interface Sent {
  text?: string;
  binary?: number[];
}

/** A socket the gateway opened; the test sends its frames. */
export class FakeGatewaySocket {
  readonly id: number;
  readonly workspace: string;
  readonly path: string;
  /** What the webview sent, in order. */
  readonly sent: Sent[] = [];
  closed: { code: number; reason: string; by: 'webview' | 'daemon' } | undefined;
  /** Sends being applied now; the webview should never have more than one out. */
  inFlight = 0;
  maxInFlight = 0;
  /** With `holdOpens`: resolves `gateway_socket_open`. */
  resolveOpen: () => void = () => {};
  readonly #channel: number;
  #index = 0;

  constructor(id: number, workspace: string, path: string, channel: number) {
    this.id = id;
    this.workspace = workspace;
    this.path = path;
    this.#channel = channel;
  }

  /** A channel message with the next index. */
  deliver(message: unknown): void {
    this.deliverAt(this.#index++, message);
  }

  /** A channel message with a given index (Tauri may deliver them out of order). */
  deliverAt(index: number, message: unknown): void {
    internals().runCallback(this.#channel, { message, index });
  }

  text(data: string): void {
    this.deliver({ type: 'text', data });
  }

  json(frame: unknown): void {
    this.text(JSON.stringify(frame));
  }

  binary(bytes: number[]): void {
    this.deliver(new Uint8Array(bytes).buffer);
  }

  /** The daemon closed it (or the connection broke: 1006). */
  close(code = 1000, reason = ''): void {
    if (this.closed !== undefined) return;
    this.closed = { code, reason, by: 'daemon' };
    this.deliver({ type: 'close', code, reason });
  }
}

export class FakeGateway {
  workspaces: GatewayWorkspace[];
  readonly daemons = new Map<string, Daemon>();
  /** Every command the webview called, with its arguments. */
  readonly calls: { cmd: string; args: Record<string, unknown> }[] = [];
  readonly sockets: FakeGatewaySocket[] = [];
  /** Called for each socket opened, e.g. to say hello on the stream. */
  onSocket: ((socket: FakeGatewaySocket) => void) | undefined;
  /** `gateway_socket_open` waits for the socket's `resolveOpen()`; frames may come before. */
  holdOpens = false;
  /** Set to answer `gateway_socket_open` with this instead of `{ socket }`. */
  openAnswer: unknown;
  /** Set to make these commands fail. */
  refuseList: { code: GatewayErrorCode; message: string } | undefined;
  refuseSends: { code: GatewayErrorCode; message: string } | undefined;
  refuseClose: { code: GatewayErrorCode; message: string } | undefined;
  #next = 1;

  constructor(workspaces: GatewayWorkspace[] = []) {
    this.workspaces = workspaces;
  }

  install(): this {
    mockIPC((cmd, args) => this.#handle(cmd, (args ?? {}) as Record<string, unknown>), { shouldMockEvents: true });
    return this;
  }

  uninstall(): void {
    clearMocks();
    Reflect.deleteProperty(window, '__TAURI_INTERNALS__');
    Reflect.deleteProperty(window, '__TAURI_EVENT_PLUGIN_INTERNALS__');
  }

  /** Changes the list and emits `gateway://workspaces`, as the gateway does. */
  async setWorkspaces(workspaces: GatewayWorkspace[]): Promise<void> {
    this.workspaces = workspaces;
    await emit('gateway://workspaces', structuredClone(workspaces));
  }

  socketsFor(workspace: string): FakeGatewaySocket[] {
    return this.sockets.filter((s) => s.workspace === workspace);
  }

  #handle(cmd: string, args: Record<string, unknown>): unknown {
    this.calls.push({ cmd, args });
    switch (cmd) {
      case 'gateway_workspaces':
        if (this.refuseList !== undefined) return failure(this.refuseList.code, this.refuseList.message);
        return structuredClone(this.workspaces);
      case 'gateway_request':
        return this.#request(args.req as GatewayRequest);
      case 'gateway_socket_open':
        return this.#open(args);
      case 'gateway_socket_send':
        return this.#send(args);
      case 'gateway_socket_close':
        return this.#close(args);
      default:
        // Tauri rejects an unknown command with a string.
        return Promise.reject(`command ${cmd} not found`);
    }
  }

  /** The workspace, if the gateway can reach its daemon; otherwise the failure. */
  #reach(id: unknown): Promise<never> | undefined {
    const workspace = this.workspaces.find((w) => w.id === id);
    if (workspace === undefined) return failure('unknown_workspace', `No workspace ${String(id)}.`);
    if (workspace.state === 'needs_pairing') return failure('needs_pairing', workspace.detail ?? 'Pair this workspace again.');
    if (workspace.state !== 'ready') return failure('unreachable', workspace.detail ?? 'The daemon does not answer.');
    return undefined;
  }

  async #request(req: GatewayRequest): Promise<GatewayResponse> {
    const refused = this.#reach(req.workspace);
    if (refused !== undefined) return refused;
    if (badPath(req.path)) return failure('invalid', `Refused path ${req.path}.`);
    if (req.body !== undefined && new TextEncoder().encode(req.body).length > 1024 * 1024) {
      return failure('too_large', 'The body is over 1 MiB.');
    }
    const daemon = this.daemons.get(req.workspace);
    if (daemon === undefined) return { status: 404, contentType: 'application/json', body: '{"code":"not_found","message":"No route."}' };
    return daemon(req);
  }

  #open(args: Record<string, unknown>): Promise<{ socket: number }> {
    const refused = this.#reach(args.workspace);
    if (refused !== undefined) return refused;
    const path = String(args.path);
    if (!SOCKET_ROUTE.test(path)) return failure('invalid', `No socket at ${path}.`);
    const channel = (args.events as { id?: unknown } | undefined)?.id;
    if (typeof channel !== 'number') return failure('invalid', 'No events channel.');
    const socket = new FakeGatewaySocket(this.#next++, String(args.workspace), path, channel);
    this.sockets.push(socket);
    // The command resolves once the upgrade succeeded; frames may follow at once.
    queueMicrotask(() => this.onSocket?.(socket));
    if (this.openAnswer !== undefined) return Promise.resolve(this.openAnswer as { socket: number });
    if (!this.holdOpens) return Promise.resolve({ socket: socket.id });
    return new Promise((resolve) => (socket.resolveOpen = () => resolve({ socket: socket.id })));
  }

  /** Each send takes a tick to apply, so a webview that does not wait would have several out. */
  async #send(args: Record<string, unknown>): Promise<null> {
    const socket = this.sockets.find((s) => s.id === args.socket);
    if (socket === undefined || socket.closed !== undefined) return failure('invalid', 'The socket is closed.');
    const { text, binary } = args;
    if ((text === undefined) === (binary === undefined)) return failure('invalid', 'Exactly one of text and binary.');
    socket.inFlight += 1;
    socket.maxInFlight = Math.max(socket.maxInFlight, socket.inFlight);
    await new Promise((done) => setTimeout(done, 0));
    socket.inFlight -= 1;
    if (this.refuseSends !== undefined) return failure(this.refuseSends.code, this.refuseSends.message);
    socket.sent.push(text === undefined ? { binary: Array.from(binary as Uint8Array | number[]) } : { text: String(text) });
    return null;
  }

  #close(args: Record<string, unknown>): Promise<null> {
    if (this.refuseClose !== undefined) return failure(this.refuseClose.code, this.refuseClose.message);
    const socket = this.sockets.find((s) => s.id === args.socket);
    if (socket !== undefined && socket.closed === undefined) {
      const code = typeof args.code === 'number' ? args.code : 1000;
      const reason = typeof args.reason === 'string' ? args.reason : '';
      socket.closed = { code, reason, by: 'webview' };
      socket.deliver({ type: 'close', code, reason });
    }
    return Promise.resolve(null);
  }
}

const json = (status: number, body: unknown): GatewayResponse => ({
  status,
  contentType: 'application/json',
  body: JSON.stringify(body),
});

export const MEMBER: Member = { id: '01JB000000000000000MEM0001', kind: 'human', handle: '@sam', name: 'Sam' };

/** A daemon serving one workspace with the given projects; every other list is empty. */
export function tinyDaemon(workspace: Workspace, projects: { id: string; key: string; name: string }[]): Daemon {
  const full: Project[] = projects.map((p) => ({
    ...p,
    status: 'in_progress',
    lead: MEMBER.id,
    members: [MEMBER.id],
    external: [],
  }));
  return (req) => {
    const route = req.path.split('?')[0];
    if (req.method !== 'GET') return json(404, { code: 'not_found', message: 'No route.' });
    if (route === '/v1/workspace') return json(200, { workspace, rev: 1 });
    if (route === '/v1/me') return json(200, MEMBER);
    if (route === '/v1/members') return json(200, [MEMBER]);
    if (route === '/v1/projects') return json(200, full);
    const project = full.find((p) => route === `/v1/projects/${p.id}`);
    if (project !== undefined) return json(200, project);
    if (/^\/v1\/[a-z]+$/.test(route ?? '')) return json(200, []);
    return json(404, { code: 'not_found', message: 'No such thing.' });
  };
}

/** Says hello on every stream socket, as a daemon does. */
export function helloOnStream(gateway: FakeGateway, rev = 1): void {
  gateway.onSocket = (socket) => {
    if (socket.path.startsWith('/v1/stream')) socket.json({ type: 'hello', rev, log: `LOG-${socket.workspace}` });
  };
}
