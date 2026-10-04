// Outward writes to GitHub and Jira, each approved by a person first (api-v1.md, "Outward
// writes: every one approved first"), by the daemon's rules, over the recorded fixtures.
//
// `pass` runs after every request that may change something, as the daemon's loop wakes on every
// append: it proposes what people's changes imply (an approval ask and `write_proposed`), records a
// denial as not sent, and "sends" an approved write (or a retried failed one) once: the fixtures'
// answer to its method and URL decides `sent` (2xx) or `failed`, and none at all is a failure too.
// A sent write changes the mock's copy of upstream (`integrations.changeUpstream`), so its next
// sync agrees. Every request "sent" is kept (`sentRequests`), so tests can show what was, and was
// not, written.

import * as integrations from './integrations.ts';
import type { Hub } from './state.ts';
import {
  APPROVAL_OPTIONS,
  WRITE_STATES,
  type Ask,
  type EventBody,
  type ExternalRef,
  type Integration,
  type MemberId,
  type Task,
  type TaskStatus,
  type UpstreamWrite,
  type WriteFields,
  type WriteProposal,
  type WriteResult,
  type Workstream,
} from './types.ts';
import { ulid } from './ulid.ts';
import { conflict, forbidden, invalid, isRecord, notFound, queryEnums, queryId } from './validate.ts';

const MAX_BODY_CHARS = 65_536;
const MAX_COMMENT_CHARS = 65_536;
const SHOWN_CHARS = 200;
const GITHUB_API = 'https://api.github.com';

/** A request the mock "sent" upstream. */
export interface SentRequest {
  method: string;
  url: string;
  body?: unknown;
}

interface State {
  /** The last revision the planner read; `undefined` until its first pass. */
  cursor: number | undefined;
  writes: UpstreamWrite[];
  retries: Set<string>;
  sent: SentRequest[];
}

const states = new WeakMap<Hub, State>();

function state(hub: Hub): State {
  let s = states.get(hub);
  if (s === undefined) {
    s = { cursor: undefined, writes: [], retries: new Set(), sent: [] };
    states.set(hub, s);
  }
  return s;
}

/** Every request "sent" upstream so far, in order. */
export function sentRequests(hub: Hub): SentRequest[] {
  return [...state(hub).sent];
}

const closed = (status: TaskStatus): boolean => status === 'done' || status === 'canceled';
const chars = (text: string): number => [...text].length;
const capped = (text: string, max: number): string => [...text].slice(0, max).join('');
const tracker = (system: string): string => (system === 'jira' ? 'Jira' : 'GitHub');

function containerOf(ref: ExternalRef): string | undefined {
  if (ref.system === 'github') return ref.key.includes('#') ? ref.key.split('#')[0] : undefined;
  if (ref.system === 'jira') {
    const dash = ref.key.lastIndexOf('-');
    return dash < 0 ? undefined : ref.key.slice(0, dash);
  }
  return undefined;
}

/** Whether the integration can write an epic (Jira Data Center needs its epic link field). */
function writesParent(integration: Integration): boolean {
  const s = integration.settings;
  return !(s.kind === 'jira' && s.deployment === 'data_center' && s.epic_link_field === undefined);
}

/** The milestone (as a link key) or epic `workstream` links in `container`. */
function parentIn(workstream: Workstream, system: string, container: string): string | undefined {
  for (const link of workstream.external) {
    if (link.system !== system) continue;
    const scope = integrations.scopeOf(link);
    if (scope?.kind === 'milestone' && scope.repo.toLowerCase() === container.toLowerCase()) {
      return `${container}#milestone:${scope.key.slice(scope.key.indexOf('#milestone:') + 11)}`;
    }
    if (scope?.kind === 'epic' && scope.project === container) return scope.key;
  }
  return undefined;
}

function setParent(fields: WriteFields, system: string, value: string | undefined): void {
  if (value === undefined) return;
  if (system === 'jira') fields.epic = value;
  else fields.milestone = value;
}

const sorted = (labels: readonly string[]): string => JSON.stringify([...labels].sort());

// ─── The ask's text ─────────────────────────────────────────────────────────────────────────────

function shown(text: string): string {
  const line = text.replace(/[\u0000-\u001f\u007f-\u009f]/gu, ' ');
  const out = capped(line, SHOWN_CHARS);
  return `"${out}${chars(line) > SHOWN_CHARS ? '…' : ''}"`;
}

function fieldLines(before: WriteFields, after: WriteFields, diff: boolean): string[] {
  const out: string[] = [];
  const labels = (l: string[]): string => (l.length === 0 ? '(none)' : l.join(', '));
  const line = (name: string, b: string | undefined, a: string): void => {
    out.push(diff ? `${name}: ${b ?? '(not read yet)'} → ${a}` : `${name}: ${a}`);
  };
  if (after.title !== undefined) line('title', before.title === undefined ? undefined : shown(before.title), shown(after.title));
  if (after.body !== undefined) line('body', before.body === undefined ? undefined : shown(before.body), shown(after.body));
  if (after.labels !== undefined) line('labels', before.labels === undefined ? undefined : labels(before.labels), labels(after.labels));
  if (after.milestone !== undefined) line('milestone', before.milestone, after.milestone);
  if (after.epic !== undefined) line('epic', before.epic, after.epic);
  if (after.state !== undefined) {
    const reason = after.close_reason === 'completed' ? ' (completed)' : after.close_reason === 'not_planned' ? ' (not planned)' : '';
    line('state', before.state, `${after.state}${reason}`);
  }
  if (after.comment !== undefined) out.push(`comment: ${shown(after.comment)}`);
  return out;
}

const FIELD_ORDER = ['title', 'body', 'labels', 'milestone', 'epic', 'state', 'close_reason', 'comment'] as const;

function askText(write: WriteProposal, taskKey: string, why: string): { title: string; body: string } {
  const name = tracker(write.system);
  const target = write.target?.key ?? write.scope;
  let title: string;
  switch (write.operation) {
    case 'create_issue':
      title = `${name}: create an issue in ${write.scope} from ${taskKey}`;
      break;
    case 'comment':
      title = `${name}: comment on ${target}`;
      break;
    case 'update': {
      const fields = FIELD_ORDER.filter((f) => write.after[f] !== undefined).map((f) =>
        f === 'body' && write.system === 'jira' ? 'description' : f,
      );
      title = `${name}: change ${fields.join(', ')} of ${target}`;
      break;
    }
    case 'close':
      title = `${name}: close ${target}`;
      break;
    case 'reopen':
      title = `${name}: reopen ${target}`;
      break;
  }
  const diff = write.operation === 'update' || write.operation === 'close' || write.operation === 'reopen';
  let body = `PitCrew sends this to ${name} only if you choose Send.\n\n`;
  for (const line of fieldLines(write.before, write.after, diff)) body += `${line}\n`;
  body += `\n${why}`;
  if (write.target?.url !== undefined) body += `\n\n${write.target.url}`;
  return { title, body };
}

// ─── Proposing ──────────────────────────────────────────────────────────────────────────────────

function propose(hub: Hub, member: MemberId, to: MemberId, write: WriteProposal, taskKey: string, why: string): UpstreamWrite | undefined {
  const s = state(hub);
  if (write.cause !== undefined && s.writes.some((w) => w.proposal.cause === write.cause)) return undefined;
  const { title, body } = askText(write, taskKey, why);
  const ask: Ask = {
    id: ulid(),
    kind: 'approval',
    from: member,
    to,
    title: capped(title, 200),
    body,
    options: [...APPROVAL_OPTIONS],
    receipts: write.cause === undefined ? [] : [{ kind: 'event', id: write.cause }],
    state: 'open',
    created: Date.now(),
  };
  if (write.task !== undefined) ask.task = write.task;
  write.ask = ask.id;
  hub.asks.push(ask);
  const raised = hub.append(member, { type: 'ask_raised', data: { ask } });
  hub.append(member, { type: 'write_proposed', data: { write } });
  const proposed: UpstreamWrite = { proposal: write, state: 'pending', attempts: 0, proposed_at: raised.at };
  s.writes.push(proposed);
  return proposed;
}

function planChange(
  hub: Hub,
  body: EventBody,
  task: Task,
  integration: Integration,
  scope: string,
  seen: integrations.Item | undefined,
): Pick<WriteProposal, 'operation' | 'before' | 'after'> | undefined {
  const system = integration.settings.kind;
  const before: WriteFields = {};
  const after: WriteFields = {};
  if (body.type === 'task_moved') {
    const { from, to } = body.data;
    if (closed(from) === closed(to)) return undefined;
    const closing = closed(to);
    if (seen !== undefined && seen.open !== closing) return undefined;
    if (seen !== undefined) before.state = closing ? 'open' : 'closed';
    after.state = closing ? 'closed' : 'open';
    if (closing && system === 'github') after.close_reason = to === 'canceled' ? 'not_planned' : 'completed';
    return { operation: closing ? 'close' : 'reopen', before, after };
  }
  if (body.type !== 'task_updated') return undefined;
  const patch = body.data.patch;
  if (patch.title !== undefined && (seen === undefined || seen.title !== patch.title)) {
    after.title = patch.title;
    if (seen !== undefined) before.title = seen.title;
  }
  if (patch.description !== undefined && (seen === undefined || seen.body !== patch.description)) {
    after.body = capped(patch.description, MAX_BODY_CHARS);
    if (seen !== undefined) before.body = seen.body;
  }
  if (patch.labels !== undefined && (seen === undefined || sorted(seen.labels) !== sorted(patch.labels))) {
    after.labels = [...patch.labels];
    if (seen !== undefined) before.labels = [...seen.labels];
  }
  if (typeof patch.workstream === 'string' && writesParent(integration)) {
    const workstream = hub.findWorkstream(patch.workstream);
    const parent = workstream === undefined ? undefined : parentIn(workstream, system, scope);
    if (parent !== undefined && seen?.parent !== parent) {
      setParent(after, system, parent);
      setParent(before, system, seen?.parent);
    }
  }
  return Object.keys(after).length === 0 ? undefined : { operation: 'update', before, after };
}

/** Proposes what the changes since the last pass imply. */
function plan(hub: Hub): void {
  const s = state(hub);
  const member = integrations.syncMember(hub);
  if (s.cursor === undefined || member === undefined) {
    s.cursor = hub.rev;
    return;
  }
  const events = hub.eventsAfter(s.cursor);
  s.cursor = hub.rev;
  for (const event of events) {
    if (event.author === member) continue;
    const body = event.body;
    if (body.type !== 'task_moved' && body.type !== 'task_updated') continue;
    const task = hub.findTaskById(body.data.task);
    const source = task?.source;
    if (task === undefined || source === undefined) continue;
    const container = containerOf(source);
    const found = container === undefined ? undefined : integrations.integrationFor(hub, source.system, container);
    if (found === undefined) continue;
    const seen = integrations.upstreamIssue(hub, found.integration, source.key);
    const planned = planChange(hub, body, task, found.integration, found.container, seen);
    if (planned === undefined) continue;
    const handle = hub.findMember(event.author)?.handle ?? 'someone';
    const why = body.type === 'task_moved' ? `Because ${handle} moved ${task.key} to ${body.data.to}.` : `Because ${handle} changed ${task.key} in PitCrew.`;
    const write: WriteProposal = {
      ask: '',
      integration: found.integration.id,
      system: found.integration.settings.kind,
      scope: found.container,
      target: source,
      task: task.id,
      ...planned,
      requested_by: event.author,
      cause: event.id,
    };
    propose(hub, member, found.integration.added_by, write, task.key, why);
    s.cursor = hub.rev;
  }
}

// ─── Sending ────────────────────────────────────────────────────────────────────────────────────

/** Why an approved write is no longer what the task says, if it is not. */
function staleReason(hub: Hub, write: WriteProposal): string | undefined {
  if (integrations.integrationById(hub, write.integration) === undefined) {
    return 'Its integration was removed; nothing was sent.';
  }
  if (write.task === undefined) return undefined;
  const task = hub.findTaskById(write.task);
  if (task === undefined) return 'Its task is gone; nothing was sent.';
  const other = `${task.key} mirrors another issue now; nothing was sent.`;
  switch (write.operation) {
    case 'create_issue':
      return task.source === undefined ? undefined : `${task.key} already mirrors an issue; nothing was sent.`;
    case 'comment':
      return task.source?.key === write.target?.key && task.source?.system === write.target?.system ? undefined : other;
    case 'close':
    case 'reopen':
      if (task.source?.key !== write.target?.key) return other;
      return closed(task.status) === (write.operation === 'close') ? undefined : `${task.key} was moved back since; nothing was sent.`;
    case 'update': {
      if (task.source?.key !== write.target?.key) return other;
      const after = write.after;
      const changed = `${task.key} changed since; nothing was sent.`;
      if (after.title !== undefined && after.title !== task.title) return changed;
      if (after.body !== undefined && after.body !== capped(task.description, MAX_BODY_CHARS)) return changed;
      if (after.labels !== undefined && JSON.stringify(after.labels) !== JSON.stringify(task.labels)) return changed;
      const parent = after.milestone ?? after.epic;
      if (parent !== undefined) {
        const workstream = task.workstream === undefined ? undefined : hub.findWorkstream(task.workstream);
        if (workstream === undefined || parentIn(workstream, write.system, write.scope) !== parent) return changed;
      }
      return undefined;
    }
  }
}

/** ADF for Jira Cloud: one paragraph per non-empty line. */
function adf(text: string): unknown {
  return {
    type: 'doc',
    version: 1,
    content: text
      .split('\n')
      .filter((l) => l.trim() !== '')
      .map((l) => ({ type: 'paragraph', content: [{ type: 'text', text: l }] })),
  };
}

function refusalMessage(system: string, status: number, body: string): string {
  let message = 'no message';
  try {
    const parsed: unknown = JSON.parse(body);
    if (isRecord(parsed)) {
      if (system === 'github' && typeof parsed['message'] === 'string') message = parsed['message'];
      if (system === 'jira') {
        const parts: string[] = [];
        if (Array.isArray(parsed['errorMessages'])) parts.push(...parsed['errorMessages'].filter((m): m is string => typeof m === 'string'));
        if (isRecord(parsed['errors'])) {
          for (const [field, m] of Object.entries(parsed['errors'])) if (typeof m === 'string') parts.push(`${field}: ${m}`);
        }
        if (parts.length > 0) message = parts.join('; ');
      }
    }
  } catch {
    // Not JSON: the fixed text.
  }
  return `${tracker(system)} refused it (${status}): ${capped(message.replace(/[\u0000-\u001f\u007f-\u009f]/gu, ' ').trim(), 300)}`;
}

/** Sends one request: the fixtures answer it, or nothing does. */
function send(hub: Hub, method: string, url: string, body?: unknown): integrations.Exchange | undefined {
  state(hub).sent.push(body === undefined ? { method, url } : { method, url, body });
  return integrations.exchange(method, url);
}

const unreachable = (url: string): WriteResult => ({
  outcome: 'failed',
  message: `request to ${url.split('?')[0] ?? url} failed: no recorded fixture matches this request`,
});

function deliver(hub: Hub, integration: Integration, write: WriteProposal): WriteResult {
  const settings = integration.settings;
  const after = write.after;
  let method: string;
  let url: string;
  let body: unknown;
  if (settings.kind === 'github') {
    const api = `${settings.api_base ?? GITHUB_API}/repos/${write.scope}`;
    const number = write.target?.key.split('#')[1];
    const milestone = after.milestone === undefined ? undefined : Number(after.milestone.slice(after.milestone.indexOf('#milestone:') + 11));
    if (write.operation === 'create_issue') {
      method = 'POST';
      url = `${api}/issues`;
      body = { title: after.title ?? '', body: after.body ?? '', labels: after.labels ?? [], ...(milestone === undefined ? {} : { milestone }) };
    } else if (write.operation === 'comment') {
      method = 'POST';
      url = `${api}/issues/${number}/comments`;
      body = { body: after.comment ?? '' };
    } else {
      method = 'PATCH';
      url = `${api}/issues/${number}`;
      const edit: Record<string, unknown> = {};
      if (after.title !== undefined) edit['title'] = after.title;
      if (after.body !== undefined) edit['body'] = after.body;
      if (after.labels !== undefined) edit['labels'] = after.labels;
      if (milestone !== undefined) edit['milestone'] = milestone;
      if (after.state !== undefined) edit['state'] = after.state;
      if (after.close_reason !== undefined) edit['state_reason'] = after.close_reason;
      body = edit;
    }
  } else {
    const api = `${settings.site}/rest/api/${settings.deployment === 'cloud' ? 3 : 2}`;
    const text = (t: string): unknown => (settings.deployment === 'cloud' ? adf(t) : t);
    const epic = (fields: Record<string, unknown>): void => {
      if (after.epic === undefined) return;
      if (settings.deployment === 'cloud') fields['parent'] = { key: after.epic };
      else if (settings.epic_link_field !== undefined) fields[settings.epic_link_field] = after.epic;
    };
    const key = write.target?.key ?? '';
    if (write.operation === 'create_issue') {
      const fields: Record<string, unknown> = { summary: after.title ?? '', description: text(after.body ?? ''), labels: after.labels ?? [] };
      epic(fields);
      fields['project'] = { key: write.scope };
      fields['issuetype'] = { name: 'Task' };
      method = 'POST';
      url = `${api}/issue`;
      body = { fields };
    } else if (write.operation === 'comment') {
      method = 'POST';
      url = `${api}/issue/${key}/comment`;
      body = { body: text(after.comment ?? '') };
    } else if (write.operation === 'update') {
      const fields: Record<string, unknown> = {};
      if (after.title !== undefined) fields['summary'] = after.title;
      if (after.body !== undefined) fields['description'] = text(after.body);
      if (after.labels !== undefined) fields['labels'] = after.labels;
      epic(fields);
      method = 'PUT';
      url = `${api}/issue/${key}`;
      body = { fields };
    } else {
      const transitions = `${api}/issue/${key}/transitions`;
      const listed = send(hub, 'GET', transitions);
      if (listed === undefined) return unreachable(transitions);
      if (listed.status < 200 || listed.status >= 300) {
        return { outcome: 'failed', message: refusalMessage('jira', listed.status, listed.body), status: listed.status };
      }
      const want = write.operation === 'close' ? 'done' : 'new';
      let id: string | undefined;
      try {
        const parsed: unknown = JSON.parse(listed.body);
        const list = isRecord(parsed) && Array.isArray(parsed['transitions']) ? parsed['transitions'] : [];
        for (const t of list) {
          const to = isRecord(t) && isRecord(t['to']) && isRecord(t['to']['statusCategory']) ? t['to']['statusCategory']['key'] : undefined;
          if (to === want && isRecord(t) && typeof t['id'] === 'string') {
            id = t['id'];
            break;
          }
        }
      } catch {
        // No transitions.
      }
      if (id === undefined) {
        return { outcome: 'failed', message: `${key}'s workflow offers no transition into ${want === 'done' ? 'Done' : 'To Do'}; nothing was sent.` };
      }
      method = 'POST';
      url = transitions;
      body = { transition: { id } };
    }
  }
  const answer = send(hub, method, url, body);
  if (answer === undefined) return unreachable(url);
  if (answer.status < 200 || answer.status >= 300) {
    return { outcome: 'failed', message: refusalMessage(settings.kind, answer.status, answer.body), status: answer.status };
  }
  let parsed: Record<string, unknown> = {};
  try {
    const value: unknown = JSON.parse(answer.body);
    if (isRecord(value)) parsed = value;
  } catch {
    // An empty answer (204).
  }
  if (settings.kind === 'github') {
    const htmlUrl = typeof parsed['html_url'] === 'string' && parsed['html_url'].startsWith('https://github.com/') ? parsed['html_url'] : undefined;
    if (write.operation === 'create_issue') {
      const number = parsed['number'];
      if (typeof number !== 'number' || number <= 0) return { outcome: 'failed', message: "GitHub's answer could not be read: no issue number; check upstream before you retry." };
      const created: ExternalRef = { system: 'github', key: `${write.scope}#${number}`, url: htmlUrl ?? `https://github.com/${write.scope}/issues/${number}` };
      return { outcome: 'sent', created, url: created.url };
    }
    return htmlUrl === undefined ? { outcome: 'sent' } : { outcome: 'sent', url: htmlUrl };
  }
  const browse = (key: string): string => `${settings.site}/browse/${key}`;
  if (write.operation === 'create_issue') {
    const key = parsed['key'];
    if (typeof key !== 'string' || !/^[A-Z][A-Z0-9_]*-[1-9][0-9]*$/.test(key)) {
      return { outcome: 'failed', message: 'Jira created the issue but did not say its key.' };
    }
    return { outcome: 'sent', created: { system: 'jira', key, url: browse(key) }, url: browse(key) };
  }
  return { outcome: 'sent', url: browse(write.target?.key ?? '') };
}

function finish(hub: Hub, write: UpstreamWrite, result: WriteResult): void {
  const member = integrations.syncMember(hub);
  if (member === undefined) return;
  const event = hub.append(member, {
    type: 'write_finished',
    data: write.proposal.task === undefined ? { ask: write.proposal.ask, result } : { ask: write.proposal.ask, task: write.proposal.task, result },
  });
  write.state = result.outcome;
  write.result = result;
  write.finished_at = event.at;
}

function sendWrite(hub: Hub, write: UpstreamWrite): void {
  const member = integrations.syncMember(hub);
  if (member === undefined) return;
  const stale = staleReason(hub, write.proposal);
  const integration = integrations.integrationById(hub, write.proposal.integration);
  if (stale !== undefined || integration === undefined) {
    finish(hub, write, { outcome: 'not_sent', reason: stale ?? 'Its integration was removed; nothing was sent.' });
    return;
  }
  write.state = 'sending';
  write.attempts += 1;
  hub.append(member, {
    type: 'write_started',
    data: write.proposal.task === undefined ? { ask: write.proposal.ask, attempt: write.attempts } : { ask: write.proposal.ask, task: write.proposal.task, attempt: write.attempts },
  });
  const missing = integrations.missingCredential(hub, integration.id);
  const result = missing === undefined ? deliver(hub, integration, write.proposal) : ({ outcome: 'failed', message: missing } as const);
  if (result.outcome === 'sent') {
    const p = write.proposal;
    const task = p.task === undefined ? undefined : hub.findTaskById(p.task);
    if (result.created !== undefined && task !== undefined && task.source === undefined) task.source = result.created;
    const key = p.target?.key;
    if (key !== undefined) {
      const change: integrations.Overlay = {};
      if (p.after.title !== undefined) change.title = p.after.title;
      if (p.after.body !== undefined) change.body = p.after.body;
      if (p.after.labels !== undefined) change.labels = p.after.labels;
      const parent = p.after.milestone ?? p.after.epic;
      if (parent !== undefined) change.parent = parent;
      if (p.after.state !== undefined) change.open = p.after.state === 'open';
      integrations.changeUpstream(hub, key, change);
    }
  }
  finish(hub, write, result);
}

/** One pass: propose, then act on answers and retries. */
export function pass(hub: Hub): void {
  plan(hub);
  const s = state(hub);
  for (const write of s.writes) {
    if (write.state !== 'pending') continue;
    const ask = hub.findAsk(write.proposal.ask);
    if (ask?.state !== 'answered' || ask.answer === undefined) continue;
    write.state = ask.answer.option === 0 ? 'approved' : 'denied';
    write.answered_at = ask.answer.at;
    write.answered_by = ask.answer.by;
  }
  for (const write of s.writes) {
    if (write.state === 'denied') {
      const name = write.answered_by === undefined ? 'the person' : (hub.findMember(write.answered_by)?.name ?? 'the person');
      finish(hub, write, { outcome: 'not_sent', reason: `Not sent: ${name} chose not to.` });
    } else if (write.state === 'approved') {
      sendWrite(hub, write);
    }
  }
  const retries = [...s.retries];
  s.retries.clear();
  for (const id of retries) {
    const write = s.writes.find((w) => w.proposal.ask === id);
    if (write?.state === 'failed') sendWrite(hub, write);
  }
}

// ─── Routes ─────────────────────────────────────────────────────────────────────────────────────

export interface Reply {
  status: number;
  body?: unknown;
}

function writeAt(hub: Hub, id: string): UpstreamWrite {
  const found = state(hub).writes.find((w) => w.proposal.ask === id.toUpperCase());
  if (found === undefined) throw notFound('No such write.');
  return found;
}

export function list(hub: Hub, query: URLSearchParams): Reply {
  const task = queryId(query, 'task', 'tsk');
  const states = queryEnums(query, 'state', WRITE_STATES);
  const writes = state(hub).writes.filter((w) => (task === undefined || w.proposal.task === task) && (states.length === 0 || states.includes(w.state)));
  return { status: 200, body: writes };
}

export function get(hub: Hub, id: string): Reply {
  return { status: 200, body: writeAt(hub, id) };
}

export function retry(hub: Hub, caller: MemberId, id: string): Reply {
  const write = writeAt(hub, id);
  const ask = hub.findAsk(write.proposal.ask);
  if (ask === undefined || ask.to !== caller) {
    throw forbidden('A person may only answer asks addressed to them or to their agents.');
  }
  if (write.state !== 'failed') {
    throw conflict(`Only a failed write is sent again; this one is ${write.state}.`);
  }
  state(hub).retries.add(write.proposal.ask);
  return { status: 202, body: write };
}

/** `POST /v1/writes`: a person asks to create an issue from a task, or to comment on its issue. */
export function request(hub: Hub, caller: MemberId, body: unknown): Reply {
  if (!isRecord(body)) throw invalid('The body must be JSON shaped as NewWrite (api-v1.md, "Outward writes").');
  const { task: taskId, operation, text } = body;
  if (typeof taskId !== 'string' || typeof operation !== 'string' || (text !== undefined && text !== null && typeof text !== 'string')) {
    throw invalid('The body must be JSON shaped as NewWrite (api-v1.md, "Outward writes").');
  }
  if (!['create_issue', 'comment', 'update', 'close', 'reopen'].includes(operation)) {
    throw invalid('The body must be JSON shaped as NewWrite (api-v1.md, "Outward writes").');
  }
  const task = hub.findTaskById(taskId);
  if (task === undefined) throw invalid(`task: no task ${taskId}.`);
  const member = integrations.syncMember(hub);
  let write: WriteProposal;
  let to: MemberId;
  if (operation === 'create_issue') {
    if (typeof text === 'string') throw invalid('text is for a comment only.');
    if (task.source !== undefined) throw conflict('This task already mirrors an issue; it cannot create another.');
    const workstream = task.workstream === undefined ? undefined : hub.findWorkstream(task.workstream);
    let found: { integration: Integration; container: string; parent: string | undefined } | undefined;
    for (const link of workstream?.external ?? []) {
      const scope = integrations.scopeOf(link);
      if (scope === undefined) continue;
      const container = scope.kind === 'repo' || scope.kind === 'milestone' ? scope.repo : scope.project;
      const hit = integrations.integrationFor(hub, link.system, container);
      if (hit === undefined) continue;
      let parent = scope.kind === 'milestone' ? `${hit.container}#milestone:${scope.key.slice(scope.key.indexOf('#milestone:') + 11)}` : scope.kind === 'epic' ? scope.key : undefined;
      if (!writesParent(hit.integration)) parent = undefined;
      found = { ...hit, parent };
      break;
    }
    if (found === undefined) {
      throw invalid('The task\'s workstream links no repository, milestone, Jira project or epic an integration syncs.');
    }
    const after: WriteFields = { title: task.title, body: capped(task.description, MAX_BODY_CHARS), labels: [...task.labels] };
    setParent(after, found.integration.settings.kind, found.parent);
    write = { ask: '', integration: found.integration.id, system: found.integration.settings.kind, scope: found.container, task: task.id, operation: 'create_issue', before: {}, after, requested_by: caller };
    to = found.integration.added_by;
  } else if (operation === 'comment') {
    const comment = typeof text === 'string' ? text : '';
    if (comment.trim() === '' || chars(comment) > MAX_COMMENT_CHARS || /[\u0000-\u0008\u000b-\u001f\u007f-\u009f]/u.test(comment)) {
      throw invalid(`text must be 1 to ${MAX_COMMENT_CHARS} characters, with no control characters but line breaks and tabs.`);
    }
    const target = task.source;
    if (target === undefined) throw conflict('This task mirrors no issue; create one from it first.');
    const container = containerOf(target);
    const hit = container === undefined ? undefined : integrations.integrationFor(hub, target.system, container);
    if (hit === undefined) throw conflict('No integration syncs the issue this task mirrors.');
    write = { ask: '', integration: hit.integration.id, system: hit.integration.settings.kind, scope: hit.container, target, task: task.id, operation: 'comment', before: {}, after: { comment }, requested_by: caller };
    to = hit.integration.added_by;
  } else {
    throw invalid('operation must be create_issue or comment; the hub proposes the others itself.');
  }
  if (member === undefined) throw conflict('No integration is connected.');
  const handle = hub.findMember(caller)?.handle ?? 'a person';
  const proposed = propose(hub, member, to, write, task.key, `Asked for by ${handle}.`);
  if (proposed === undefined) throw new Error('a request has no cause, so it is never a duplicate');
  return { status: 201, body: proposed };
}
