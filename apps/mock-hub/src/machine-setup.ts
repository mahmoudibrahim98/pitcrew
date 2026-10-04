// Machine setup (api-v1.md, "Machine setup"): what the daemon would answer for a synthetic
// laptop. The check is fixed (Claude Code and Codex there, OpenCode and gh missing, no SLURM), the
// accounts are what the "CLIs" would say, and a sign-in is a canned terminal that ends by itself
// after `delays.signIn` (or when the person presses Enter in it), after which that CLI reports
// `sam@example.com`. The mock runs nothing and reads nothing.
//
// The rules are the daemon's: device tokens only (the route table says so), the hub's own machine
// only (its first `local` one; another is 409), `engine` is one of the three (404 otherwise), one
// sign-in per CLI at a time (asking again while one runs answers it, 200), 409 for a CLI that is
// not installed, `device_code` for Codex only (400 for the others), unknown body fields 400.

import type { Hub } from './state.ts';
import type { Engine, MachineId } from './types.ts';
import { ulid } from './ulid.ts';
import { conflict, invalid, isRecord, notFound } from './validate.ts';
import type { WebSocketConnection } from './ws.ts';

export type MachineCheckItem =
  | 'cli_claude'
  | 'cli_codex'
  | 'cli_opencode'
  | 'tmux'
  | 'git'
  | 'gh'
  | 'disk'
  | 'slurm'
  | 'helper';

export interface MachineCheckRow {
  id: MachineCheckItem;
  status: 'ok' | 'warn' | 'missing';
  detail: string;
  version?: string;
  fix?: 'install_page' | 'install_helper';
}

export interface AgentAccount {
  engine: Engine;
  installed: boolean;
  signed_in?: boolean;
  account?: string;
  detail?: string;
}

export interface SignIn {
  engine: Engine;
  terminal: string;
  command: string[];
  running: boolean;
  started: number;
}

const CHECK_ITEMS: readonly MachineCheckItem[] = [
  'cli_claude',
  'cli_codex',
  'cli_opencode',
  'tmux',
  'git',
  'gh',
  'disk',
  'slurm',
  'helper',
];

const SETUP_ENGINES: readonly Engine[] = ['claude', 'codex', 'opencode'];

const LABEL: Record<Engine, string> = { claude: 'Claude Code', codex: 'Codex', opencode: 'OpenCode' };

/** The synthetic laptop's rows, in the daemon's order. A fresh array each time. */
function rows(): MachineCheckRow[] {
  return [
    { id: 'cli_claude', status: 'ok', detail: '2.1.3 (Claude Code)', version: '2.1.3 (Claude Code)' },
    { id: 'cli_codex', status: 'ok', detail: 'codex-cli 0.50.0', version: 'codex-cli 0.50.0' },
    { id: 'cli_opencode', status: 'missing', detail: 'OpenCode (opencode) is not on PATH.', fix: 'install_page' },
    { id: 'tmux', status: 'ok', detail: 'tmux 3.4', version: 'tmux 3.4' },
    { id: 'git', status: 'ok', detail: 'git version 2.43.0', version: 'git version 2.43.0' },
    { id: 'gh', status: 'missing', detail: 'Not found on PATH: only GitHub’s integration needs it.', fix: 'install_page' },
    { id: 'disk', status: 'ok', detail: '128 GB free' },
  ];
}

const LOGIN: Record<Engine, Partial<Record<'browser' | 'device_code', string[]>>> = {
  claude: { browser: ['claude', 'auth', 'login'] },
  codex: { browser: ['codex', 'login'], device_code: ['codex', 'login', '--device-auth'] },
  opencode: { browser: ['opencode', 'auth', 'login'] },
};

interface SignInRecord {
  view: SignIn;
  /** Open terminals, told when the login ends. */
  ended: Set<() => void>;
}

interface Setup {
  signedIn: Set<Engine>;
  signIns: Map<Engine, SignInRecord>;
}

const setups = new WeakMap<Hub, Setup>();

function setupOf(hub: Hub): Setup {
  let setup = setups.get(hub);
  if (setup === undefined) {
    setup = { signedIn: new Set(['codex']), signIns: new Map() };
    setups.set(hub, setup);
  }
  return setup;
}

/** The hub's own machine, or why `id` is not it. */
function ownMachine(hub: Hub, id: string): MachineId {
  const machine = hub.findMachine(id);
  if (machine === undefined) {
    throw notFound(`No machine ${id}.`);
  }
  const own = hub.machines.find((m) => m.kind === 'local');
  if (own?.id !== machine.id) {
    throw conflict(
      `${machine.name} is not this hub's own machine: set it up through its own hub, or while connecting it.`,
    );
  }
  return machine.id;
}

function engineOf(name: string): Engine {
  if (!(SETUP_ENGINES as readonly string[]).includes(name)) {
    throw notFound(`No agent CLI "${name}".`);
  }
  return name as Engine;
}

function installed(engine: Engine): boolean {
  return engine !== 'opencode';
}

/** `GET /v1/machines/{id}/check[?row=]`. */
export function checkMachine(hub: Hub, id: string, query: URLSearchParams): { rows: MachineCheckRow[] } {
  const row = query.get('row');
  if (row !== null && !(CHECK_ITEMS as readonly string[]).includes(row)) {
    throw invalid(`No check row "${row}".`);
  }
  ownMachine(hub, id);
  const all = rows();
  return { rows: row === null ? all : all.filter((r) => r.id === row) };
}

/** `GET /v1/machines/{id}/agents`. */
export function agentAccounts(hub: Hub, id: string): AgentAccount[] {
  ownMachine(hub, id);
  const { signedIn } = setupOf(hub);
  return SETUP_ENGINES.map((engine): AgentAccount => {
    if (!installed(engine)) {
      return { engine, installed: false, detail: `${LABEL[engine]} (${engine}) is not on PATH.` };
    }
    if (!signedIn.has(engine)) return { engine, installed: true, signed_in: false };
    return { engine, installed: true, signed_in: true, account: engine === 'codex' ? 'ChatGPT' : 'sam@example.com' };
  });
}

/** `GET /v1/machines/{id}/agents/{engine}/sign-in`. */
export function signInStatus(hub: Hub, id: string, name: string): SignIn {
  const engine = engineOf(name);
  ownMachine(hub, id);
  const found = setupOf(hub).signIns.get(engine);
  if (found === undefined) {
    throw notFound(`No sign-in to ${LABEL[engine]} is open here.`);
  }
  return { ...found.view };
}

/** `POST /v1/machines/{id}/agents/{engine}/sign-in`: the answer's status and body. */
export function startSignIn(hub: Hub, id: string, name: string, body: unknown): { status: number; body: SignIn } {
  let method: 'browser' | 'device_code' = 'browser';
  if (body !== undefined) {
    if (!isRecord(body) || Object.keys(body).some((key) => key !== 'method')) {
      throw invalid('The body must be {} or {"method": "browser" | "device_code"}.');
    }
    const given = body['method'];
    if (given !== undefined) {
      if (given !== 'browser' && given !== 'device_code') {
        throw invalid('The body must be {} or {"method": "browser" | "device_code"}.');
      }
      method = given;
    }
  }
  const engine = engineOf(name);
  ownMachine(hub, id);
  const command = LOGIN[engine][method];
  if (command === undefined) {
    throw invalid(`${LABEL[engine]} has no device-code sign-in; its login shows what to do.`);
  }
  if (!installed(engine)) {
    throw conflict(`${LABEL[engine]} (${engine}) is not on this machine's PATH: install it first.`);
  }
  const setup = setupOf(hub);
  const running = setup.signIns.get(engine);
  if (running?.view.running === true) {
    return { status: 200, body: { ...running.view } };
  }
  const record: SignInRecord = {
    view: { engine, terminal: ulid(), command: [...command], running: true, started: Date.now() },
    ended: new Set(),
  };
  setup.signIns.set(engine, record);
  hub.later(hub.delays.signIn, () => finish(setup, engine, record));
  return { status: 201, body: { ...record.view } };
}

function finish(setup: Setup, engine: Engine, record: SignInRecord): void {
  if (!record.view.running) return;
  record.view.running = false;
  setup.signedIn.add(engine);
  for (const tell of record.ended) tell();
  record.ended.clear();
}

/** The sign-in whose terminal `ref` is, if any. */
export function signInTerminal(hub: Hub, ref: string): { engine: Engine; record: SignInRecord } | undefined {
  const setup = setups.get(hub);
  if (setup === undefined) return undefined;
  for (const [engine, record] of setup.signIns) {
    if (record.view.terminal === ref) return { engine, record };
  }
  return undefined;
}

/**
 * Serves a sign-in's terminal: a canned login screen, the person's keys echoed, Enter finishing
 * the login. Once it has ended, `exit` and a close, as for a session that ends.
 */
export function openSignInTerminal(hub: Hub, conn: WebSocketConnection, found: { engine: Engine; record: SignInRecord }): void {
  const setup = setupOf(hub);
  const { engine, record } = found;
  conn.sendBinary(
    Buffer.from(
      `\x1b[2J\x1b[H\x1b[1m${record.view.command.join(' ')}\x1b[0m\r\n` +
        `\x1b[2mMock sign-in: nothing is sent anywhere. Press Enter to finish it.\x1b[0m\r\n` +
        'Open https://example.com/login and paste the code here.\r\nCode: ',
      'utf8',
    ),
  );
  const exit = (): void => {
    conn.sendBinary(Buffer.from('\r\nSigned in as sam@example.com.\r\n', 'utf8'));
    conn.sendText(JSON.stringify({ type: 'exit' }));
    conn.close(1000, 'sign-in ended');
  };
  if (!record.view.running) {
    exit();
    return;
  }
  record.ended.add(exit);
  conn.onBinary = (keys) => {
    const enter = keys.indexOf(0x0d);
    conn.sendBinary(enter === -1 ? keys : keys.subarray(0, enter));
    if (enter !== -1) finish(setup, engine, record);
  };
  conn.onText = () => {};
  void conn.closed.then(() => record.ended.delete(exit));
}
