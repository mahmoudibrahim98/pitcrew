// A fake desktop gateway with the remote commands and SSH's prompts: Tauri's IPC mocked with
// `mockIPC` (events included), answering docs/build/contracts/desktop-gateway.md's commands. Each
// workspace's daemon is a function from a request to a response; stream sockets say hello; the
// remote commands do what the test sets. Every command is recorded with its arguments. Needs a DOM
// (happy-dom) for `window`. For tests only: it is the test's own stand-in for the Rust gateway.

import { emit } from '@tauri-apps/api/event';
import { clearMocks, mockIPC } from '@tauri-apps/api/mocks';
import type { GatewayRequest, GatewayResponse } from '../gateway.ts';
import type { Member, Workspace } from '../types.ts';
import type { GatewayWorkspace } from '../workspaces.tsx';

export type Daemon = (req: GatewayRequest) => GatewayResponse | Promise<GatewayResponse>;

interface Internals {
  runCallback(id: number, data: unknown): void;
}

function internals(): Internals {
  return (window as unknown as { __TAURI_INTERNALS__: Internals }).__TAURI_INTERNALS__;
}

/** A gateway failure, as the gateway rejects a command. */
export function refuse(code: string, message: string): Promise<never> {
  return Promise.reject({ code, message });
}

/** Sends messages on a channel, in order. */
export class FakeChannel {
  readonly #id: number;
  #index = 0;

  constructor(id: number) {
    this.#id = id;
  }

  send(message: unknown): void {
    internals().runCallback(this.#id, { message, index: this.#index++ });
  }
}

function channelOf(value: unknown): FakeChannel {
  const id = (value as { id?: unknown } | undefined)?.id;
  if (typeof id !== 'number') throw new Error('no channel');
  return new FakeChannel(id);
}

export interface FakeDesktopOptions {
  workspaces?: GatewayWorkspace[];
}

export class FakeDesktop {
  workspaces: GatewayWorkspace[];
  readonly daemons = new Map<string, Daemon>();
  /** Every command the webview called, with its arguments. */
  readonly calls: { cmd: string; args: Record<string, unknown> }[] = [];
  /** What `gateway_ssh_hosts` answers. */
  hosts: unknown = { hosts: [] };
  probe: (host: string) => unknown = (host) => ({ host, os: 'linux', arch: 'x86_64' });
  plan: (req: unknown) => unknown = () => ({ plan: 'plan-1', steps: ['Copy pitcrewd to ~/.pitcrew'] });
  /** `gateway_remote_add`: progress goes on `channel`; resolve with the workspace, or `refuse`. */
  add: (plan: string, channel: FakeChannel) => Promise<unknown> = () => refuse('invalid', 'No add set up.');
  remove: (workspace: string, stopHelper: boolean) => Promise<unknown> = () => Promise.resolve(null);
  /** Called with each `gateway_prompt_reply`'s arguments. */
  onReply: ((args: Record<string, unknown>) => void) | undefined;

  constructor(options: FakeDesktopOptions = {}) {
    this.workspaces = options.workspaces ?? [];
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

  commands(cmd: string): Record<string, unknown>[] {
    return this.calls.filter((c) => c.cmd === cmd).map((c) => c.args);
  }

  async setWorkspaces(workspaces: GatewayWorkspace[]): Promise<void> {
    this.workspaces = workspaces;
    await emit('gateway://workspaces', structuredClone(workspaces));
  }

  /** Emits `gateway://prompt` with `payload`, as given (the point is to test the webview's checks). */
  async prompt(payload: unknown): Promise<void> {
    await emit('gateway://prompt', payload);
  }

  async closePrompt(id: unknown): Promise<void> {
    await emit('gateway://prompt-closed', { id });
  }

  /** Resolves with the next `gateway_prompt_reply`'s arguments. */
  nextReply(): Promise<Record<string, unknown>> {
    return new Promise((resolve) => {
      const before = this.onReply;
      this.onReply = (args) => {
        this.onReply = before;
        before?.(args);
        resolve(args);
      };
    });
  }

  #handle(cmd: string, args: Record<string, unknown>): unknown {
    this.calls.push({ cmd, args });
    switch (cmd) {
      case 'gateway_workspaces':
        return structuredClone(this.workspaces);
      case 'gateway_request':
        return this.#request(args.req as GatewayRequest);
      case 'gateway_socket_open':
        return this.#open(args);
      case 'gateway_socket_send':
      case 'gateway_socket_close':
        return null;
      case 'gateway_ssh_hosts':
        return this.hosts;
      case 'gateway_remote_probe':
        return this.probe(String(args.host));
      case 'gateway_remote_plan':
        return this.plan(args.req);
      case 'gateway_remote_add':
        return this.add(String(args.plan), channelOf(args.events));
      case 'gateway_workspace_remove':
        return this.remove(String(args.workspace), args.stopHelper === true);
      case 'gateway_prompt_reply':
        this.onReply?.(args);
        return null;
      default:
        return Promise.reject(`command ${cmd} not found`);
    }
  }

  async #request(req: GatewayRequest): Promise<GatewayResponse> {
    const workspace = this.workspaces.find((w) => w.id === req.workspace);
    if (workspace === undefined) return refuse('unknown_workspace', `No workspace ${req.workspace}.`);
    if (workspace.state !== 'ready') return refuse('unreachable', 'The daemon does not answer.');
    const daemon = this.daemons.get(req.workspace);
    if (daemon === undefined) return json(404, { code: 'not_found', message: 'No route.' });
    return daemon(req);
  }

  #open(args: Record<string, unknown>): Promise<{ socket: number }> {
    const workspace = this.workspaces.find((w) => w.id === args.workspace);
    if (workspace === undefined || workspace.state !== 'ready') return refuse('unreachable', 'The daemon does not answer.');
    const channel = channelOf(args.events);
    if (String(args.path).startsWith('/v1/stream')) {
      queueMicrotask(() => channel.send({ type: 'text', data: JSON.stringify({ type: 'hello', rev: 1, log: 'LOG' }) }));
    }
    return Promise.resolve({ socket: this.calls.length });
  }
}

export const json = (status: number, body: unknown): GatewayResponse => ({
  status,
  contentType: 'application/json',
  body: JSON.stringify(body),
});

/** The member a fresh daemon's device token becomes at setup. */
export const ME_ID = '01JB000000000000000MEM0001';

export interface FreshDaemon {
  daemon: Daemon;
  /** The setup bodies the daemon accepted. */
  setups: unknown[];
  readonly me: Member | undefined;
}

/**
 * A daemon serving a fresh workspace (api-v1.md, "The first run"): `setup_needed` until
 * `POST /v1/setup`, `GET /v1/me` 404 until then, and `409` after. Every other list is empty.
 */
export function freshDaemon(id: string): FreshDaemon {
  const workspace: Workspace = { id, name: '' };
  let me: Member | undefined;
  const members: Member[] = [];
  const machines: unknown[] = [];
  const setups: unknown[] = [];
  const daemon: Daemon = (req) => {
    const route = req.path.split('?')[0];
    if (req.method === 'POST' && route === '/v1/setup') {
      if (me !== undefined) return json(409, { code: 'conflict', message: 'This workspace is already set up.' });
      const body = JSON.parse(req.body ?? '{}') as { workspace_name: string; person: { name: string; handle: string }; machine_name: string };
      setups.push(body);
      workspace.name = body.workspace_name;
      me = { id: ME_ID, kind: 'human', handle: body.person.handle, name: body.person.name };
      members.push(me);
      const machine = { id: '01JB000000000000000MAC0001', name: body.machine_name, kind: 'local', liveness: 'live' };
      machines.push(machine);
      return json(200, { workspace, me, machine });
    }
    if (req.method !== 'GET') return json(404, { code: 'not_found', message: 'No route.' });
    if (route === '/v1/workspace') return json(200, { workspace, rev: 1, ...(me === undefined ? { setup_needed: true } : {}) });
    if (route === '/v1/me') return me === undefined ? json(404, { code: 'not_found', message: 'No member yet.' }) : json(200, me);
    if (route === '/v1/members') return json(200, members);
    if (route === '/v1/machines') return json(200, machines);
    if (/^\/v1\/[a-z]+$/.test(route ?? '')) return json(200, []);
    return json(404, { code: 'not_found', message: 'No such thing.' });
  };
  return {
    daemon,
    setups,
    get me() {
      return me;
    },
  };
}
