// Machine setup on the wire (api-v1.md, "Machine setup"; `pitcrew_protocol::machine_setup`), as
// the hub and the desktop gateway send it, and its mapping to this feature's types. Every answer is
// read, not trusted (a remote hub, or a remote machine's check, is another machine's word): a row
// or an account that does not have the contract's shape is dropped, and text is cut to a line.

import type { Engine } from '../data/index.ts';
import type { AgentAccount, CheckFix, CheckRowId, MachineCheckRow, StartSignInResult } from './api.ts';

export type WireCheckItem =
  | 'cli_claude'
  | 'cli_codex'
  | 'cli_opencode'
  | 'tmux'
  | 'git'
  | 'gh'
  | 'disk'
  | 'slurm'
  | 'helper';

export interface WireCheckRow {
  id: WireCheckItem;
  status: 'ok' | 'warn' | 'missing';
  detail: string;
  version?: string;
  fix?: 'install_page' | 'install_helper';
}

export interface WireMachineCheck {
  rows: WireCheckRow[];
}

export interface WireAgentAccount {
  engine: Engine;
  installed: boolean;
  signed_in?: boolean;
  account?: string;
  detail?: string;
}

export interface WireSignIn {
  engine: Engine;
  terminal: string;
  command: string[];
  running: boolean;
  started: number;
}

const ITEMS: Record<WireCheckItem, { id: CheckRowId; label: string }> = {
  cli_claude: { id: 'cli-claude', label: 'Claude Code CLI' },
  cli_codex: { id: 'cli-codex', label: 'Codex CLI' },
  cli_opencode: { id: 'cli-opencode', label: 'OpenCode CLI' },
  tmux: { id: 'tmux', label: 'tmux' },
  git: { id: 'git', label: 'git' },
  gh: { id: 'gh', label: 'GitHub CLI (gh)' },
  disk: { id: 'disk', label: 'Disk space' },
  slurm: { id: 'slurm', label: 'SLURM' },
  helper: { id: 'helper', label: 'PitCrew helper' },
};

const FIXES: Record<NonNullable<WireCheckRow['fix']>, CheckFix> = {
  install_page: 'install-page',
  install_helper: 'install-helper',
};

const ENGINES: readonly Engine[] = ['claude', 'codex', 'opencode'];
const ULID = /^[0-9A-HJKMNP-TV-Z]{26}$/;
/** The longest text kept from the wire; the hub's own are shorter. */
const MAX_TEXT = 200;

/** The wire name of a row (`cli-claude` → `cli_claude`). */
export function wireItem(id: CheckRowId): WireCheckItem {
  return id.replace('-', '_') as WireCheckItem;
}

function isRecord(value: unknown): value is Record<string, unknown> {
  return typeof value === 'object' && value !== null && !Array.isArray(value);
}

/** C0 and C1 controls, and the line and paragraph separators. */
function isControl(char: string): boolean {
  const code = char.codePointAt(0) ?? 0;
  return code < 0x20 || (code >= 0x7f && code <= 0x9f) || code === 0x2028 || code === 0x2029;
}

/**
 * Characters that change how a line reads without being seen: direction marks, embeddings,
 * overrides (U+202E) and isolates, zero-width characters, fillers, variation selectors and tag
 * characters. The same table as `pitcrew_protocol::text::is_hidden`, which the hub's own check
 * (`clean`) applies (T92); the line and paragraph separators, also in it, are spaces here.
 */
const HIDDEN: readonly (readonly [number, number])[] = [
  [0x00ad, 0x00ad],
  [0x034f, 0x034f],
  [0x061c, 0x061c],
  [0x115f, 0x1160],
  [0x180e, 0x180e],
  [0x200b, 0x200f],
  [0x202a, 0x202e],
  [0x2060, 0x2064],
  [0x2066, 0x2069],
  [0x3164, 0x3164],
  [0xfe00, 0xfe0f],
  [0xfeff, 0xfeff],
  [0xffa0, 0xffa0],
  [0xfff9, 0xfffb],
  [0xe0000, 0xe007f],
  [0xe0100, 0xe01ef],
];

function isHidden(char: string): boolean {
  const code = char.codePointAt(0) ?? 0;
  return HIDDEN.some(([from, to]) => code >= from && code <= to);
}

/**
 * One plain line: control characters and line breaks made spaces, hidden characters dropped, at
 * most `MAX_TEXT` characters. A remote hub or machine is another machine's word (T92).
 */
export function line(text: string): string {
  const plain = [...text]
    .map((char) => (isControl(char) ? ' ' : isHidden(char) ? '' : char))
    .join('')
    .replace(/\s+/g, ' ')
    .trim();
  return [...plain].slice(0, MAX_TEXT).join('');
}

function optionalText(value: unknown): string | undefined | null {
  if (value === undefined) return undefined;
  return typeof value === 'string' ? line(value) : null;
}

/** A wire row, or `undefined` when it is not one. */
export function toCheckRow(value: unknown): MachineCheckRow | undefined {
  if (!isRecord(value)) return undefined;
  const id = value['id'];
  // Own keys only: `constructor` or `__proto__` name no row.
  const item = typeof id === 'string' && Object.hasOwn(ITEMS, id) ? ITEMS[id as WireCheckItem] : undefined;
  const status = value['status'];
  if (item === undefined || (status !== 'ok' && status !== 'warn' && status !== 'missing')) return undefined;
  if (typeof value['detail'] !== 'string') return undefined;
  const wireFix = value['fix'];
  if (wireFix !== undefined && (typeof wireFix !== 'string' || !Object.hasOwn(FIXES, wireFix))) return undefined;
  const fix = wireFix === undefined ? undefined : FIXES[wireFix as keyof typeof FIXES];
  const detail = line(value['detail']);
  return {
    id: item.id,
    label: item.label,
    status,
    ...(detail === '' ? {} : { detail }),
    fixable: fix !== undefined,
    ...(fix === undefined ? {} : { fix }),
  };
}

/** The rows of a `MachineCheck`, in the order given, without any that are malformed or repeated. */
export function toCheckRows(value: unknown): MachineCheckRow[] {
  if (!isRecord(value) || !Array.isArray(value['rows'])) throw new Error('The machine check answered something else.');
  const rows: MachineCheckRow[] = [];
  for (const raw of value['rows']) {
    const row = toCheckRow(raw);
    if (row !== undefined && !rows.some((r) => r.id === row.id)) rows.push(row);
  }
  return rows;
}

/** An `AgentAccount`, or `undefined` when it is not one. */
export function toAccount(value: unknown): AgentAccount | undefined {
  if (!isRecord(value)) return undefined;
  const engine = value['engine'];
  if (typeof engine !== 'string' || !(ENGINES as readonly string[]).includes(engine)) return undefined;
  if (typeof value['installed'] !== 'boolean') return undefined;
  const signedIn = value['signed_in'];
  if (signedIn !== undefined && typeof signedIn !== 'boolean') return undefined;
  const account = optionalText(value['account']);
  const detail = optionalText(value['detail']);
  if (account === null || detail === null) return undefined;
  return {
    engine: engine as Engine,
    installed: value['installed'],
    signedIn,
    ...(account === undefined || account === '' ? {} : { account }),
    ...(detail === undefined || detail === '' ? {} : { detail }),
  };
}

/** Every account given, one per engine. */
export function toAccounts(value: unknown): AgentAccount[] {
  if (!Array.isArray(value)) throw new Error('The agents’ accounts answered something else.');
  const accounts: AgentAccount[] = [];
  for (const raw of value) {
    const account = toAccount(raw);
    if (account !== undefined && !accounts.some((a) => a.engine === account.engine)) accounts.push(account);
  }
  return accounts;
}

/** A `SignIn`, checked. */
export function toSignIn(value: unknown): WireSignIn {
  const bad = new Error('The sign-in answered something else.');
  if (!isRecord(value)) throw bad;
  const { engine, terminal, command, running, started } = value;
  if (typeof engine !== 'string' || !(ENGINES as readonly string[]).includes(engine)) throw bad;
  if (typeof terminal !== 'string' || !ULID.test(terminal)) throw bad;
  if (!Array.isArray(command) || !command.every((word) => typeof word === 'string')) throw bad;
  if (typeof running !== 'boolean' || typeof started !== 'number') throw bad;
  return { engine: engine as Engine, terminal, command: command.map((w: string) => line(w)), running, started };
}

export function toStartSignInResult(signIn: WireSignIn): StartSignInResult {
  return { terminalSessionId: signIn.terminal, command: signIn.command };
}
