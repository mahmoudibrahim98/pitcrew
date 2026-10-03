// A simulated desktop app for Playwright: an init script that gives the page Tauri's internals and
// plays the desktop gateway (docs/build/contracts/desktop-gateway.md) in the page itself, so the
// UI runs its real desktop data layer (`src/data/desktop.tsx`, `gateway.ts`) in a real browser.
//
// - Requests and sockets go to one mock hub (`hubUrl`) with its token: the gateway's job, which
//   this script stands in for. The UI itself still never holds a token.
// - The remote commands script one remote, `hpc-login`, with SLURM: the probe asks to confirm its
//   host key, and the add asks for a password, then registers the hub as a workspace.
// - What the UI sent is recorded on `window.__fakeDesktop` for the spec to read: the commands, and
//   each prompt reply's shape (whether it had an answer, and its length; never the answer).
//
// Passed to `page.addInitScript(installFakeDesktop, options)`, which sends it as source text: it
// must not use anything from outside its own body.

export interface FakeDesktopOptions {
  hubUrl: string;
  token: string;
  /** The SLURM job script the plan shows, verbatim. */
  jobScript: string;
  initialWorkspace?: { id: string; name: string; host: string; kind: 'remote'; state: 'ready' };
}

export interface FakeDesktopRecord {
  calls: string[];
  replies: { id: string; answered: boolean; length: number; accept?: boolean }[];
  adds: string[];
}

export function installFakeDesktop({ hubUrl, token, jobScript, initialWorkspace }: FakeDesktopOptions): void {
  type Callback = (data: unknown) => void;
  const callbacks = new Map<number, Callback>();
  const listeners = new Map<string, Set<number>>();
  const sockets = new Map<number, WebSocket>();
  let nextCallback = 1;
  let nextSocket = 1;
  let plans = 0;
  let workspaces: unknown[] = initialWorkspace === undefined ? [] : [initialWorkspace];
  let pending: { id: string; resolve: (args: Record<string, unknown>) => void } | undefined;
  const record: FakeDesktopRecord = { calls: [], replies: [], adds: [] };
  (window as unknown as { __fakeDesktop: FakeDesktopRecord }).__fakeDesktop = record;

  const fail = (code: string, message: string) => Promise.reject({ code, message });

  function transformCallback(callback: Callback | undefined, once = false): number {
    const id = nextCallback++;
    callbacks.set(id, (data) => {
      if (once) callbacks.delete(id);
      callback?.(data);
    });
    return id;
  }

  function runCallback(id: number, data: unknown): void {
    callbacks.get(id)?.(data);
  }

  function emit(event: string, payload: unknown): void {
    for (const handler of listeners.get(event) ?? []) runCallback(handler, { event, id: 0, payload });
  }

  /** A channel's sender: messages in order, as Tauri numbers them. */
  function channel(value: unknown): (message: unknown) => void {
    const id = (value as { id: number }).id;
    let index = 0;
    return (message) => runCallback(id, { message, index: index++ });
  }

  /** Emits a prompt and waits for its reply. */
  function ask(prompt: { id: string; host: string; kind: string; text: string; fingerprint?: string }) {
    return new Promise<Record<string, unknown>>((resolve) => {
      pending = { id: prompt.id, resolve };
      emit('gateway://prompt', prompt);
    });
  }

  async function request(req: { method: string; path: string; body?: string }) {
    const headers: Record<string, string> = { Authorization: `Bearer ${token}` };
    if (req.body !== undefined) headers['Content-Type'] = 'application/json';
    const res = await fetch(`${hubUrl}${req.path}`, { method: req.method, headers, body: req.body ?? null });
    return { status: res.status, contentType: res.headers.get('content-type') ?? undefined, body: await res.text() };
  }

  function open(args: { path: string; events: unknown }) {
    const send = channel(args.events);
    return new Promise<{ socket: number }>((resolve, reject) => {
      const socket = new WebSocket(`${hubUrl.replace(/^http/, 'ws')}${args.path}`, ['pitcrew.v1', `pitcrew.bearer.${token}`]);
      socket.binaryType = 'arraybuffer';
      const id = nextSocket++;
      let opened = false;
      socket.onopen = () => {
        opened = true;
        sockets.set(id, socket);
        resolve({ socket: id });
      };
      socket.onmessage = (event) => send(typeof event.data === 'string' ? { type: 'text', data: event.data } : event.data);
      socket.onclose = (event) => {
        sockets.delete(id);
        if (opened) send({ type: 'close', code: event.code, reason: event.reason });
        else reject({ code: 'unreachable', message: `The hub refused the socket (${event.code}).` });
      };
    });
  }

  // As the gateway does: each progress message names one of the plan's steps, and the last is the
  // whole add's (`add`).
  const COPY = 'Copy pitcrewd 0.4.0 to ~/.pitcrew';
  const SUBMIT = 'Submit the job below';
  const PAIR = 'Connect and pair';

  async function add(args: { plan: string; events: unknown }) {
    record.adds.push(args.plan);
    const progress = channel(args.events);
    progress({ step: COPY, state: 'running' });
    progress({ step: COPY, state: 'running', detail: '40% sent' });
    const reply = await ask({ id: 'pw-1', host: 'hpc-login', kind: 'password', text: "sam@hpc-login's password: " });
    if (typeof reply.answer !== 'string') {
      progress({ step: COPY, state: 'failed', detail: 'Authentication cancelled.' });
      progress({ step: 'add', state: 'failed', detail: 'ssh: authentication cancelled' });
      return fail('unreachable', 'ssh: authentication cancelled');
    }
    progress({ step: COPY, state: 'done' });
    progress({ step: SUBMIT, state: 'running', detail: 'job 4242 pending (Priority)' });
    progress({ step: SUBMIT, state: 'done', detail: 'Submitted batch job 4242' });
    const info = (await (await fetch(`${hubUrl}/v1/workspace`, { headers: { Authorization: `Bearer ${token}` } })).json()) as {
      workspace: { id: string };
    };
    const workspace = { id: info.workspace.id, name: 'hpc-login', host: 'login.example.org', kind: 'remote', state: 'ready' };
    workspaces = [workspace];
    emit('gateway://workspaces', workspaces);
    progress({ step: PAIR, state: 'done' });
    progress({ step: 'add', state: 'done' });
    return workspace;
  }

  async function invoke(cmd: string, args: Record<string, unknown> = {}): Promise<unknown> {
    record.calls.push(cmd);
    switch (cmd) {
      case 'plugin:event|listen': {
        const event = String(args.event);
        if (!listeners.has(event)) listeners.set(event, new Set());
        listeners.get(event)?.add(args.handler as number);
        return args.handler;
      }
      case 'plugin:event|unlisten':
        listeners.get(String(args.event))?.delete(args.eventId as number);
        return null;
      case 'gateway_workspaces':
        return workspaces;
      case 'gateway_request':
        return request(args.req as { method: string; path: string; body?: string });
      case 'gateway_socket_open':
        return open(args as { path: string; events: unknown });
      case 'gateway_socket_send': {
        const socket = sockets.get(args.socket as number);
        if (socket === undefined) return fail('invalid', 'The socket is closed.');
        socket.send(typeof args.text === 'string' ? args.text : new Uint8Array(args.binary as ArrayLike<number>));
        return null;
      }
      case 'gateway_socket_close': {
        const code = typeof args.code === 'number' && (args.code === 1000 || args.code >= 3000) ? args.code : 1000;
        sockets.get(args.socket as number)?.close(code);
        return null;
      }
      case 'gateway_ssh_hosts':
        return { hosts: ['hpc-login', 'build-box'] };
      case 'gateway_remote_probe': {
        const host = String(args.host);
        const reply = await ask({
          id: 'hk-1',
          host,
          kind: 'host_key',
          text: `The authenticity of host '${host}' can't be established.`,
          fingerprint: 'SHA256:bWFkZS11cC1rZXktZm9yLXRlc3RzLW9ubHktMDAwMQ',
        });
        if (reply.accept !== true) return fail('unreachable', 'Host key verification failed.');
        return {
          host,
          os: 'linux',
          arch: 'x86_64',
          slurm: { version: '23.02.7', defaultPartition: 'gpu', srunOverlap: true },
          tmux: { version: '3.4' },
        };
      }
      case 'gateway_remote_plan': {
        plans += 1;
        const req = args.req as { launcher: string };
        // The add below plays the SLURM plan's steps.
        return {
          plan: `plan-${plans}`,
          steps: [COPY, req.launcher === 'slurm' ? SUBMIT : `Start it with ${req.launcher}`, PAIR],
          ...(req.launcher === 'slurm' ? { jobScript } : {}),
        };
      }
      case 'gateway_remote_add':
        return add(args as { plan: string; events: unknown });
      case 'gateway_workspace_remove':
        workspaces = workspaces.filter((w) => (w as { id: string }).id !== args.workspace);
        emit('gateway://workspaces', workspaces);
        return null;
      case 'gateway_prompt_reply': {
        const id = String(args.id);
        record.replies.push({
          id,
          answered: typeof args.answer === 'string',
          length: typeof args.answer === 'string' ? args.answer.length : 0,
          ...(typeof args.accept === 'boolean' ? { accept: args.accept } : {}),
        });
        if (pending?.id === id) {
          const { resolve } = pending;
          pending = undefined;
          resolve(args);
        }
        return null;
      }
      default:
        return Promise.reject(`command ${cmd} not found`);
    }
  }

  Object.assign(window, {
    __TAURI_INTERNALS__: {
      invoke,
      transformCallback,
      unregisterCallback: (id: number) => callbacks.delete(id),
      runCallback,
      callbacks,
    },
    __TAURI_EVENT_PLUGIN_INTERNALS__: {
      unregisterListener: (event: string, id: number) => {
        listeners.get(event)?.delete(id);
        callbacks.delete(id);
      },
    },
  });
}
