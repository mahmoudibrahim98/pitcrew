// Board drafts (api-v1.md, "Board drafts"): an agent drafts a workstream's board from its
// history, and nothing is created until a person reviews the proposal.
//
// The mock builds the summary as the hub does (the same template, `crates/office/prompts/`, the
// same bounds and the same redaction rules, ported), from its sessions and its recaps fixture.
// It plays the back office: a draft run by `@office` gets a synthetic proposal (a task per session
// it summarised) once its session is working; any other agent's draft waits for that agent's
// `POST /v1/board-drafts/{id}/proposal` (the dev agent token is @writer's).

import { createHash } from 'node:crypto';
import { readFileSync } from 'node:fs';
import { includedRecaps, includesSession } from './import.ts';
import { announceSession, createSession } from './simulate.ts';
import type { Hub } from './state.ts';
import {
  ENGINES,
  TASK_STATUSES,
  type BoardDraft,
  type BoardProposal,
  type DraftCost,
  type DraftPreview,
  type DraftedTask,
  type Engine,
  type Machine,
  type Member,
  type ProposedTask,
  type Session,
  type Task,
  type TaskStatus,
  type Workstream,
} from './types.ts';
import { isUlid, ulid } from './ulid.ts';
import { ApiFailure, Fields, conflict, forbidden, invalid, notFound, unavailable } from './validate.ts';

/** The prompt template the hub builds into its binary. */
const TEMPLATE = readFileSync(
  new URL('../../../crates/office/prompts/draft-board/v1.md', import.meta.url),
  'utf8',
);
const PROMPT = 'draft-board/v1';
const PLACEHOLDER_ID = 'drf_00000000000000000000000000';

// The bounds (crates/office/src/board.rs and crates/protocol/src/board.rs).
const MAX_SESSIONS = 40;
const MAX_TASKS = 60;
const MAX_TASKS_BYTES = 4 * 1024;
const MAX_SUMMARY_BYTES = 12 * 1024;
const MAX_NAME_CHARS = 80;
const MAX_TITLE_CHARS = 120;
const MAX_RECAP_LINES = 3;
const MAX_RECAP_CHARS = 200;
const MAX_FILES = 5;
const MAX_PATH_CHARS = 100;
const CLI_OVERHEAD_TOKENS = 15_000;
export const MAX_PROPOSAL_BYTES = 32 * 1024;
const MAX_PROPOSED_TASKS = 50;
const MAX_PROPOSED_TITLE = 200;
const MAX_PROPOSED_DESCRIPTION = 2000;
const MAX_EVIDENCE = 20;
const MAX_NOTE = 2000;
export const DRAFTED_LABEL = 'drafted';

/** Each hub's drafts, oldest first. */
const DRAFTS = new WeakMap<Hub, BoardDraft[]>();

function draftsOf(hub: Hub): BoardDraft[] {
  let drafts = DRAFTS.get(hub);
  if (drafts === undefined) {
    drafts = [];
    DRAFTS.set(hub, drafts);
  }
  return drafts;
}

// ─── Redaction (crates/office/src/redact.rs) ───────────────────────────────────────────────────

const REDACTED = '[redacted]';
const EMAIL = '[email]';
const PREFIXES = [
  'sk-', 'sk_live_', 'sk_test_', 'rk_live_', 'rk_test_', 'pk_live_', 'ghp_', 'gho_', 'ghu_', 'ghs_',
  'ghr_', 'github_pat_', 'glpat-', 'gldt-', 'xoxa-', 'xoxb-', 'xoxp-', 'xoxr-', 'xoxs-', 'xapp-',
  'AKIA', 'ASIA', 'AIza', 'ya29.', 'pcd_', 'pca_', 'npm_', 'pypi-', 'hf_', 'dop_v1_', 'doo_v1_',
  'shpat_', 'shpss_', 'SG.', 'glc_', 'sq0atp-', 'EAAC', 'ATATT',
];
const SECRET_NAMES = [
  'password', 'passwd', 'passphrase', 'secret', 'token', 'apikey', 'api_key', 'api-key', 'access_key',
  'access-key', 'private_key', 'private-key', 'credential', 'authorization', 'session_key', 'cookie',
];
const SECRET_EXACT = ['pwd', 'pw', 'pat', 'key', 'sig', 'signature'];
const SCHEMES = ['bearer', 'basic', 'digest'];
const HIDDEN =
  /[­͏؜ᅟᅠ᠎​-‏‪-‮⁠-⁤⁦-⁩ㅤ︀-️﻿ﾠ￹-￻]|\uDB40[\uDC00-\uDDEF]/gu;
const GAP = /[\s"'`()[\]{}<>,;|]/u;

function isSecretName(name: string): boolean {
  const lower = name.replace(/^\$+/, '').toLowerCase();
  return SECRET_EXACT.includes(lower) || SECRET_NAMES.some((s) => lower.includes(s));
}

function isEmail(word: string): boolean {
  const at = word.lastIndexOf('@');
  if (at <= 0) return false;
  const local = word.slice(0, at).replace(/^[<(:]+/, '');
  const domain = word.slice(at + 1);
  return (
    local !== '' &&
    !local.includes('/') &&
    /^[\p{L}\p{N}._%+\-!#$&*=^{}~]+$/u.test(local) &&
    domain.includes('.') &&
    !domain.startsWith('.') &&
    !domain.endsWith('.') &&
    /^[\p{L}\p{N}.-]+$/u.test(domain)
  );
}

function isToken(word: string): boolean {
  for (const prefix of PREFIXES) {
    if (!word.startsWith(prefix)) continue;
    const run = /^[A-Za-z0-9_\-+/=.]*/.exec(word.slice(prefix.length))?.[0] ?? '';
    if (run.length >= 8 && (/\d/.test(run) || (/[A-Z]/.test(run) && /[a-z]/.test(run)))) return true;
  }
  const parts = word.split('.');
  if (
    parts.length === 3 &&
    parts[0]?.startsWith('eyJ') === true &&
    word.length >= 30 &&
    parts.every((p) => /^[A-Za-z0-9_=-]+$/.test(p))
  ) {
    return true;
  }
  for (const part of word.split(/[.:@]/)) {
    if (part.length < 32) continue;
    if (/^[0-9a-fA-F]+$/.test(part) && /\d/.test(part) && /[a-fA-F]/.test(part)) return true;
    const longestRun = Math.max(...part.split(/[/_-]/).map((p) => p.length));
    if (/^[A-Za-z0-9+/=_-]+$/.test(part) && /[A-Z]/.test(part) && /[a-z]/.test(part) && /\d/.test(part) && longestRun >= 20) {
      return true;
    }
  }
  return false;
}

interface Count {
  n: number;
}

function replacement(part: string): string | undefined {
  if (part === '') return undefined;
  if (isEmail(part)) return EMAIL;
  if (isToken(part)) return REDACTED;
  return undefined;
}

function scrub(part: string, count: Count): string {
  const whole = replacement(part);
  if (whole !== undefined) {
    count.n += 1;
    return whole;
  }
  if (!/[/\\]/.test(part)) return part;
  return part
    .split(/([/\\])/)
    .map((segment) => {
      if (segment === '/' || segment === '\\') return segment;
      const r = replacement(segment);
      if (r === undefined) return segment;
      count.n += 1;
      return r;
    })
    .join('');
}

function pairs(word: string, count: Count): string {
  return word
    .split(/([&?#])/)
    .map((part) => {
      if (part === '&' || part === '?' || part === '#') return part;
      const eq = part.indexOf('=');
      if (eq === -1) return scrub(part, count);
      const name = part.slice(0, eq);
      const value = part.slice(eq + 1);
      if (value !== '' && isSecretName(name)) {
        count.n += 1;
        return `${name}=${REDACTED}`;
      }
      return `${scrub(name, count)}=${scrub(value, count)}`;
    })
    .join('');
}

function wordRules(core: string, count: Count): string {
  if (core === '') return '';
  if (isEmail(core)) {
    count.n += 1;
    return EMAIL;
  }
  const scheme = /^([^:]*):\/\/(.*)$/s.exec(core);
  if (scheme !== null) {
    const rest = scheme[2] ?? '';
    const hostEnd = rest.search(/[/?#]/);
    const host = hostEnd === -1 ? rest : rest.slice(0, hostEnd);
    const at = host.lastIndexOf('@');
    if (at !== -1) {
      count.n += 1;
      return pairs(`${scheme[1]}://${REDACTED}${rest.slice(at)}`, count);
    }
  }
  if (core.includes('=')) return pairs(core, count);
  const colon = core.indexOf(':');
  if (colon !== -1) {
    const value = core.slice(colon + 1);
    if (value !== '' && !value.startsWith('//') && isSecretName(core.slice(0, colon))) {
      count.n += 1;
      return `${core.slice(0, colon)}:${REDACTED}`;
    }
  }
  return scrub(core, count);
}

function asksForValue(core: string, tail: string): boolean {
  const lower = core.toLowerCase();
  if (SCHEMES.includes(lower)) return true;
  const named = lower.replace(/[=:]+$/, '');
  const option = named.replace(/^-+/, '');
  const endsNamed = tail.startsWith(':') || core.endsWith('=') || core.endsWith(':');
  return (endsNamed || named.length !== option.length) && option !== '' && isSecretName(option);
}

function homes(text: string, count: Count): string {
  return text.replace(
    /(^|[\s"'`()[\]{}<>,;|=:@])(\/home\/[^/\s"'`()[\]{}<>,;|]+|\/[Uu]sers\/[^/\s"'`()[\]{}<>,;|]+|[A-Za-z]:[\\/][Uu][Ss][Ee][Rr][Ss][\\/][^\\/\s"'`()[\]{}<>,;|]+|\/root(?=[/\s]|$))/g,
    (_, before: string) => {
      count.n += 1;
      return `${before}~`;
    },
  );
}

/** One clean line of at most `max` characters, redacted as the hub redacts it. */
export function redactLine(text: string, max: number): { text: string; count: number } {
  const limit = max * 4 + 64;
  let tidy = text.replace(HIDDEN, '').replace(/[\s\p{Cc}  ]+/gu, ' ').trim();
  const chars = [...tidy];
  const cut = chars.length > limit;
  if (cut) tidy = chars.slice(0, limit).join('');
  if (tidy.includes('PRIVATE KEY') && tidy.includes('-----BEGIN')) return { text: REDACTED, count: 1 };
  const count: Count = { n: 0 };
  let out = '';
  let valueNext = false;
  for (const piece of tidy.split(/([\s"'`()[\]{}<>,;|]+)/u)) {
    if (piece === '' || GAP.test(piece[0] ?? '')) {
      out += piece;
      continue;
    }
    const core = piece.replace(/[.:!?]+$/, '');
    const tail = piece.slice(core.length);
    const scheme = SCHEMES.includes(core.toLowerCase());
    if (valueNext && !scheme && [...core].length >= 3 && !core.startsWith('-')) {
      count.n += 1;
      out += REDACTED;
    } else {
      out += wordRules(core, count);
    }
    valueNext = asksForValue(core, tail);
    out += tail;
  }
  out = homes(out, count);
  const all = [...out];
  if (all.length > max || (cut && all.length === max)) {
    out = `${all.slice(0, max - 1).join('').trimEnd()}…`;
  } else if (cut) {
    out += '…';
  }
  return { text: out, count: count.n };
}

// ─── The summary (crates/office/src/board.rs) ──────────────────────────────────────────────────

function cleaner() {
  const total = { n: 0 };
  return {
    total,
    line(text: string, max: number): string {
      const r = redactLine(text, max);
      total.n += r.count;
      return r.text.replaceAll('<', '‹').replaceAll('>', '›');
    },
  };
}

const ENGINE_WORD: Record<Engine, string> = { claude: 'Claude Code', codex: 'Codex', opencode: 'OpenCode' };

const day = (at: number): string => new Date(at).toISOString().slice(0, 10);

/** The sessions a draft of `workstream` summarises. */
function draftSessions(hub: Hub, workstream: Workstream): Session[] {
  const drafting = new Set(draftsOf(hub).map((d) => d.session));
  return hub.sessions.filter(
    (s) =>
      s.workstream === workstream.id &&
      s.parent === undefined &&
      !drafting.has(s.id) &&
      includesSession(hub.importChoice, s),
  );
}

function sessionBlock(hub: Hub, session: Session, keys: Map<string, string>, clean: ReturnType<typeof cleaner>): string {
  const blocks = includedRecaps(hub)
    .blocks.filter((b) => b.block.session === session.id)
    .sort((a, b) => (a.block.id < b.block.id ? 1 : -1))
    .slice(0, 50);
  let turns = 0;
  let tools = 0;
  let failed = 0;
  let edits = 0;
  const files = new Map<string, number>();
  const recaps: string[] = [];
  for (const { block, line } of blocks) {
    turns += block.counts.turns;
    tools += block.counts.tools_run;
    failed += block.counts.tools_failed;
    edits += block.counts.file_edits;
    for (const touch of block.files) files.set(touch.path, (files.get(touch.path) ?? 0) + touch.edits);
    if (recaps.length < MAX_RECAP_LINES && line.text.trim() !== '') recaps.push(line.text);
  }
  const from = day(session.started);
  const to = day(session.last_activity);
  let facts = `  ${ENGINE_WORD[session.engine]}, ${session.state}, ${from === to ? from : `${from} to ${to}`}`;
  if (session.branch !== undefined) facts += `, branch ${clean.line(session.branch, MAX_PATH_CHARS)}`;
  const key = session.task === undefined ? undefined : keys.get(session.task);
  if (key !== undefined) facts += `, task ${clean.line(key, MAX_NAME_CHARS)}`;
  let out = `Session ${session.id}\n${facts}\n`;
  if (session.title !== undefined && session.title.trim() !== '') {
    out += `  Title: ${clean.line(session.title, MAX_TITLE_CHARS)}\n`;
  }
  out += `  Work: ${turns} turns, ${tools} tool runs (${failed} failed), ${edits} file edits\n`;
  const sorted = [...files].sort((a, b) => b[1] - a[1] || (a[0] < b[0] ? -1 : 1)).map(([p]) => p);
  const shown = sorted.slice(0, MAX_FILES).map((f) => clean.line(f, MAX_PATH_CHARS)).filter((f) => f !== '');
  if (shown.length > 0) {
    const more = sorted.length - MAX_FILES;
    out += `  Files: ${shown.join(', ')}${more > 0 ? ` and ${more} more` : ''}\n`;
  }
  const lines = recaps.map((r) => clean.line(r, MAX_RECAP_CHARS)).filter((r) => r !== '');
  if (lines.length > 0) {
    out += '  Recent work:\n';
    for (const l of lines) out += `  - ${l}\n`;
  }
  return out;
}

const bytes = (text: string): number => Buffer.byteLength(text);

/** The prompt for `workstream` naming `draft`, its summary and its cost. */
function makePrompt(hub: Hub, workstream: Workstream, draft: string) {
  const clean = cleaner();
  const tasks = hub.tasks.filter((t) => t.workstream === workstream.id);
  const keys = new Map(tasks.map((t) => [t.id, t.key]));
  let tasksText = '';
  let listed = 0;
  for (const task of tasks.slice(0, MAX_TASKS)) {
    const line = `- ${clean.line(task.key, MAX_NAME_CHARS)} [${task.status}] ${clean.line(task.title, MAX_TITLE_CHARS)}\n`;
    if (bytes(tasksText) + bytes(line) > MAX_TASKS_BYTES) break;
    tasksText += line;
    listed += 1;
  }
  if (listed === 0) tasksText += '(none)\n';
  if (tasks.length > listed) tasksText += `(and ${tasks.length - listed} more not listed)\n`;
  const sessions = draftSessions(hub, workstream).sort(
    (a, b) => b.last_activity - a.last_activity || (a.id < b.id ? 1 : -1),
  );
  const blocks: string[] = [];
  let used = bytes(tasksText) + 400;
  for (const session of sessions.slice(0, MAX_SESSIONS)) {
    const block = sessionBlock(hub, session, keys, clean);
    if (used + bytes(block) + 1 > MAX_SUMMARY_BYTES) break;
    used += bytes(block) + 1;
    blocks.push(block);
  }
  const leftOut = sessions.length - blocks.length;
  let summary = `Sessions: ${blocks.length} of ${sessions.length}, most recently active first${
    leftOut > 0 ? `; the ${leftOut} least recently active are left out` : ''
  }.\n`;
  summary += `\nTasks already on the board (${listed} of ${tasks.length}):\n${tasksText}`;
  if (blocks.length === 0) summary += '\n(No sessions are linked to this workstream yet.)\n';
  for (const block of blocks) summary += `\n${block}`;
  summary = summary.trimEnd();
  const project = hub.findProject(workstream.project);
  const values: Record<string, string> = {
    workstream: clean.line(workstream.name, MAX_NAME_CHARS),
    project: clean.line(project?.name ?? '', MAX_NAME_CHARS),
    draft,
    max_tasks: String(MAX_PROPOSED_TASKS),
    max_title: String(MAX_PROPOSED_TITLE),
    summary,
  };
  const text = TEMPLATE.replace(/\{\{([^{}]*)\}\}/g, (whole, name: string) => values[name] ?? whole);
  const promptBytes = bytes(text);
  const cost: DraftCost = {
    sessions: blocks.length,
    sessions_left_out: leftOut,
    tasks: listed,
    summary_bytes: bytes(summary),
    prompt_bytes: promptBytes,
    redacted: clean.total.n,
    estimate: {
      input_tokens: CLI_OVERHEAD_TOKENS + Math.ceil(promptBytes / 4),
      output_tokens: MAX_PROPOSAL_BYTES / 4,
    },
  };
  return { text, summary, cost, sessions: sessions.slice(0, blocks.length) };
}

const digestOf = (text: string): string => createHash('sha256').update(text).digest('hex');

// ─── Lookups ────────────────────────────────────────────────────────────────────────────────────

function workstreamAt(hub: Hub, id: string): Workstream {
  const found = hub.findWorkstream(id);
  if (found === undefined) throw notFound(`No workstream ${id}.`);
  return found;
}

function bareDraftId(ref: string): string {
  return (ref.startsWith('drf_') ? ref.slice(4) : ref).toUpperCase();
}

function stateOf(hub: Hub, draft: BoardDraft): BoardDraft {
  if (draft.state === 'running') {
    const session = hub.findSession(draft.session);
    if (session === undefined || session.state === 'ended') return { ...draft, state: 'ended' };
  }
  return draft;
}

/** The draft `ref` as it stands (its state follows its session), or a 404. */
export function draftAt(hub: Hub, ref: string): BoardDraft {
  const id = bareDraftId(ref);
  const found = isUlid(id) ? draftsOf(hub).find((d) => d.id === id) : undefined;
  if (found === undefined) throw notFound(`No board draft ${ref}.`);
  return stateOf(hub, found);
}

function stored(hub: Hub, id: string): BoardDraft {
  const found = draftsOf(hub).find((d) => d.id === id);
  if (found === undefined) throw notFound(`No board draft ${id}.`);
  return found;
}

// ─── Handlers ───────────────────────────────────────────────────────────────────────────────────

/** `GET /v1/workstreams/{id}/board-draft`. */
export function boardPreview(hub: Hub, workstreamId: string): DraftPreview {
  const workstream = workstreamAt(hub, workstreamId);
  const made = makePrompt(hub, workstream, PLACEHOLDER_ID);
  return { workstream: workstream.id, prompt: PROMPT, cost: made.cost, summary: made.summary, digest: digestOf(made.text) };
}

/** Where the draft runs: the workstream's first location, the project's root, or the hub's `~`. */
function placeOf(hub: Hub, workstream: Workstream): { machine: Machine; path: string; branch?: string | undefined } {
  const root = hub.findProject(workstream.project)?.root;
  const location = [...workstream.locations, ...(root ? [root] : [])][0];
  const machine =
    location === undefined ? hub.machines.find((m) => m.kind === 'local') : hub.findMachine(location.machine);
  if (machine === undefined) throw unavailable('No machine can run the draft.');
  if (machine.liveness !== 'live') throw unavailable(`${machine.name} is ${machine.liveness}; its runner cannot be reached.`);
  return { machine, path: location?.path ?? '~', branch: location?.branch };
}

/** `POST /v1/workstreams/{id}/board-drafts`. */
export function startDraft(hub: Hub, me: string, workstreamId: string, body: unknown): BoardDraft {
  const workstream = workstreamAt(hub, workstreamId);
  const fields = new Fields(body);
  const agentId = fields.optString('agent');
  const engineGiven = fields.optEnum('engine', ENGINES);
  const digest = fields.string('digest');
  let agent: Member | undefined;
  if (agentId === undefined) {
    agent = hub.members.find((m) => m.handle === '@office' && m.owner === me);
    if (agent === undefined) throw invalid('Name an agent to draft the board: this hub has no back office of yours.');
  } else {
    agent = hub.findMember(agentId);
    if (agent === undefined) throw invalid(`agent: no member ${agentId}.`);
  }
  const chosen: Member = agent;
  if (chosen.kind !== 'agent') throw invalid(`agent must be an agent; ${chosen.handle} is a person.`);
  if (chosen.owner !== me) {
    throw forbidden(`${chosen.handle} is not your agent: a person may run only their own agents.`);
  }
  const made = makePrompt(hub, workstream, PLACEHOLDER_ID);
  if (digestOf(made.text) !== digest) {
    throw conflict('The workstream has changed since its preview: preview it again, and confirm what will be sent.');
  }
  const open = draftsOf(hub)
    .map((d) => stateOf(hub, d))
    .find((d) => d.workstream === workstream.id && (d.state === 'running' || d.state === 'proposed'));
  if (open !== undefined) {
    throw conflict(`${workstream.name} already has a board draft ${open.state === 'running' ? 'running' : 'waiting for review'}.`);
  }
  const place = placeOf(hub, workstream);
  const persona = chosen.persona === undefined ? undefined : hub.findPersona(chosen.persona);
  const engine = engineGiven ?? persona?.engine ?? 'claude';
  const id = ulid();
  const session = createSession(hub, {
    engine,
    machine: place.machine.id,
    cwd: place.path,
    branch: place.branch,
    title: `Drafting the board of ${workstream.name}`,
    agent: chosen.id,
    workstream: workstream.id,
    link_basis: 'manual',
    brief: made.text.replace(PLACEHOLDER_ID, `drf_${id}`),
  });
  const draft: BoardDraft = {
    id,
    workstream: workstream.id,
    agent: chosen.id,
    engine,
    session: session.id,
    by: me,
    prompt: PROMPT,
    cost: made.cost,
    started: session.started,
    state: 'running',
    accepted: [],
    rejected: [],
  };
  draftsOf(hub).push(draft);
  announceSession(hub, session);
  hub.append(me, {
    type: 'board_draft_started',
    data: { draft: id, workstream: workstream.id, agent: chosen.id, engine, session: session.id, prompt: PROMPT, cost: made.cost },
  });
  if (chosen.handle === '@office') {
    // The mock's back office: once its session works, a task for each session it summarised.
    const sessions = made.sessions;
    hub.later(hub.delays.start + hub.delays.reply, () => {
      const now = stateOf(hub, draft);
      if (now.state !== 'running') return;
      const tasks: ProposedTask[] = sessions.slice(0, MAX_PROPOSED_TASKS).map((s) => ({
        title: redactLine(s.title ?? `Follow up on session ${s.id}`, MAX_PROPOSED_TITLE).text,
        status: s.state === 'ended' ? 'review' : 'in_progress',
        evidence: [s.id],
      }));
      record(hub, chosen.id, draft, { tasks, note: 'Drafted by the mock back office from the session titles.' });
    });
  }
  return draft;
}

/** `GET /v1/board-drafts?workstream=`, newest first; `workstream` a bare id. */
export function listDrafts(hub: Hub, workstream: string | undefined): BoardDraft[] {
  return draftsOf(hub)
    .filter((d) => workstream === undefined || d.workstream === workstream.toUpperCase())
    .map((d) => stateOf(hub, d))
    .reverse();
}

function checkedProposal(hub: Hub, draft: BoardDraft, body: unknown): BoardProposal {
  if (Buffer.byteLength(JSON.stringify(body ?? null)) > MAX_PROPOSAL_BYTES) {
    throw invalid(`The proposal is larger than ${MAX_PROPOSAL_BYTES / 1024} KiB.`);
  }
  const fields = new Fields(body);
  const raw = fields.optArray('tasks');
  if (raw === undefined) throw invalid('tasks is required');
  if (raw.length > MAX_PROPOSED_TASKS) throw invalid(`tasks: at most ${MAX_PROPOSED_TASKS}.`);
  const workstream = hub.findWorkstream(draft.workstream);
  const allowed = new Set(workstream === undefined ? [] : draftSessions(hub, workstream).map((s) => s.id));
  const tasks = raw.map((value, i): ProposedTask => {
    const task = new Fields(value, `tasks[${i}]`);
    const title = task.string('title').trim();
    if ([...title].length === 0 || [...title].length > MAX_PROPOSED_TITLE) {
      throw invalid(`tasks[${i}].title must be 1 to ${MAX_PROPOSED_TITLE} characters.`);
    }
    const status: TaskStatus = task.enumOf('status', TASK_STATUSES);
    if (status === 'canceled') throw invalid(`tasks[${i}].status: a draft proposes work, never a canceled task.`);
    const description = task.optString('description')?.trim();
    if (description !== undefined && [...description].length > MAX_PROPOSED_DESCRIPTION) {
      throw invalid(`tasks[${i}].description must be at most ${MAX_PROPOSED_DESCRIPTION} characters.`);
    }
    const evidence = [...new Set((task.optStringArray('evidence') ?? []).map((e) => e.replace(/^ses_/, '').toUpperCase()))];
    if (evidence.length > MAX_EVIDENCE) throw invalid(`tasks[${i}].evidence: at most ${MAX_EVIDENCE} sessions.`);
    for (const session of evidence) {
      if (!allowed.has(session)) throw invalid(`tasks[${i}].evidence: ${session} is not one of the workstream's sessions.`);
    }
    const out: ProposedTask = { title: redactLine(title, MAX_PROPOSED_TITLE).text, status, evidence };
    const d = description === undefined || description === '' ? undefined : redactLine(description, MAX_PROPOSED_DESCRIPTION).text;
    if (d !== undefined && d !== '') out.description = d;
    return out;
  });
  const note = fields.optString('note')?.trim();
  if (note !== undefined && [...note].length > MAX_NOTE) throw invalid(`note must be at most ${MAX_NOTE} characters.`);
  const proposal: BoardProposal = { tasks };
  if (note !== undefined && note !== '') proposal.note = redactLine(note, MAX_NOTE).text;
  return proposal;
}

function record(hub: Hub, author: string, draft: BoardDraft, proposal: BoardProposal): void {
  const target = stored(hub, draft.id);
  target.proposal = proposal;
  target.proposed = Date.now();
  target.state = 'proposed';
  hub.append(author, {
    type: 'board_proposed',
    data: { draft: draft.id, workstream: draft.workstream, tasks: proposal.tasks, ...(proposal.note === undefined ? {} : { note: proposal.note }) },
  });
}

/** `POST /v1/board-drafts/{id}/proposal`: the drafting agent's only. */
export function proposeBoard(hub: Hub, caller: { memberId: string; scope: string }, ref: string, body: unknown): BoardDraft {
  const draft = draftAt(hub, ref);
  if (caller.scope !== 'agent' || caller.memberId !== draft.agent) {
    throw forbidden(`Only the agent drafting drf_${draft.id} may propose its board.`);
  }
  const proposal = checkedProposal(hub, draft, body);
  if (draft.state === 'proposed' || draft.state === 'reviewed') throw conflict(`drf_${draft.id} already has a proposal.`);
  if (draft.state === 'ended') throw conflict(`drf_${draft.id} has ended: its session ended without a proposal.`);
  record(hub, caller.memberId, draft, proposal);
  return draftAt(hub, draft.id);
}

/** The project's next task key. */
function nextKey(hub: Hub, project: { id: string; key: string }, offset: number): string {
  const prefix = `${project.key}-`;
  const highest = hub.tasks
    .filter((t) => t.project === project.id && t.key.startsWith(prefix))
    .map((t) => Number(t.key.slice(prefix.length)))
    .reduce((max, n) => (Number.isSafeInteger(n) && n > max ? n : max), 0);
  return `${prefix}${highest + 1 + offset}`;
}

/** `POST /v1/board-drafts/{id}/review`: the accepted items become tasks; the rest, nothing. */
export function reviewDraft(hub: Hub, me: string, ref: string, body: unknown): { draft: BoardDraft; tasks: Task[] } {
  const draft = draftAt(hub, ref);
  const fields = new Fields(body);
  const accept = fields.optArray('accept');
  if (accept === undefined) throw invalid('accept is required');
  if (!accept.every((item) => typeof item === 'number' && Number.isSafeInteger(item) && item >= 0)) {
    throw invalid('accept must list whole numbers.');
  }
  if (draft.state === 'reviewed') throw conflict(`drf_${draft.id} was reviewed already.`);
  if (draft.state !== 'proposed' || draft.proposal === undefined) throw conflict(`drf_${draft.id} has no proposal to review yet.`);
  const proposal = draft.proposal;
  const seen = new Set<number>();
  for (const item of accept as number[]) {
    if (item >= proposal.tasks.length) throw invalid(`accept: ${item} is not one of the proposal's ${proposal.tasks.length} tasks.`);
    if (seen.has(item)) throw invalid(`accept: ${item} is given twice.`);
    seen.add(item);
  }
  const workstream = hub.findWorkstream(draft.workstream);
  const project = workstream === undefined ? undefined : hub.findProject(workstream.project);
  if (workstream === undefined || project === undefined) throw new ApiFailure('conflict', "The draft's workstream is no longer known.");
  const accepted = [...seen].sort((a, b) => a - b);
  const created: Task[] = [];
  const drafted: DraftedTask[] = [];
  const linked = new Set<string>();
  const links: { session: Session; task: Task }[] = [];
  accepted.forEach((item, i) => {
    const proposed = proposal.tasks[item]!;
    const task: Task = {
      id: ulid(),
      key: nextKey(hub, project, i),
      project: project.id,
      workstream: workstream.id,
      title: proposed.title,
      description: proposed.description ?? '',
      status: proposed.status,
      priority: 'none',
      labels: [DRAFTED_LABEL],
      blocked_by: [],
      accept_auto: false,
      subtasks: [],
    };
    created.push(task);
    drafted.push({ item, task: task.id });
    for (const id of proposed.evidence) {
      const session = hub.findSession(id);
      if (session !== undefined && session.task === undefined && session.workstream === workstream.id && !linked.has(id)) {
        linked.add(id);
        links.push({ session, task });
      }
    }
  });
  const rejected = proposal.tasks.map((_, i) => i).filter((i) => !seen.has(i));
  for (const task of created) {
    hub.tasks.push(task);
    hub.append(me, { type: 'task_created', data: { task } });
  }
  for (const { session, task } of links) {
    session.task = task.id;
    session.link_basis = 'manual';
    hub.append(me, { type: 'session_linked', data: { session: session.id, workstream: workstream.id, task: task.id, basis: 'manual' } });
  }
  const target = stored(hub, draft.id);
  target.state = 'reviewed';
  target.reviewed = Date.now();
  target.accepted = drafted;
  target.rejected = rejected;
  hub.append(me, { type: 'board_draft_reviewed', data: { draft: draft.id, workstream: workstream.id, accepted: drafted, rejected } });
  return { draft: draftAt(hub, draft.id), tasks: created };
}
