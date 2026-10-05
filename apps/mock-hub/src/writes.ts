// Outward writes to GitHub and Jira, each approved by a person first (api-v1.md, "Outward
// writes: every one approved first"), by the daemon's rules, over the recorded fixtures.
//
// `pass` runs after every request that may change something, as the daemon's loop wakes on every
// append: it proposes what people's changes imply (an approval ask from the integration's own sync
// member, and `write_proposed`), records a denial as not sent, and "sends" an approved write (or a
// failed one a person asked to retry) once:
// - only what the hub holds exactly is proposed: a title or description upstream holds lossily
//   (hidden characters, cut, Jira rich text) is left out, and labels go as a change;
// - before an edit, close or reopen, the issue is read as upstream has it now (its fixture, with
//   what sent writes changed laid over it): what upstream already holds is not sent, and a field
//   it changed since the proposal sends nothing;
// - a retried create or comment first looks for its earlier attempt in the fixtures;
// - the fixtures' answer to each request decides `sent` (2xx) or `failed`, and none at all is a
//   failure too.
// A sent write changes the mock's copy of upstream (`integrations.changeUpstream`), so its next
// sync agrees. Every request "sent", reads included, is kept (`sentRequests`), so tests can show
// what was, and was not, written.

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
const CUT_OFF = 'The hub stopped while sending; a retry first looks upstream for this attempt.';
/** How long before a write's approval its earlier attempt is looked for (clock skew). */
const EARLIER_MARGIN_MS = 10 * 60 * 1000;

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
  sent: SentRequest[];
}

const states = new WeakMap<Hub, State>();

function state(hub: Hub): State {
  let s = states.get(hub);
  if (s === undefined) {
    s = { cursor: undefined, writes: [], sent: [] };
    states.set(hub, s);
  }
  return s;
}

/** Every request "sent" upstream so far, reads included, in order. */
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

const parentOf = (fields: WriteFields): string | undefined => fields.milestone ?? fields.epic;

/** One label as the hub holds it (the daemon's `fit_labels` of it alone). */
const held = (label: string): string | undefined => integrations.heldLabels([label])[0];

/**
 * The labels `labels` (the task's) adds to and removes from upstream's `seen` as the hub holds
 * them; labels the hub does not hold are in neither (the daemon's `label_change`).
 */
function labelChange(seen: string[], labels: string[]): { add: string[]; remove: string[] } {
  const hub = integrations.heldLabels(seen);
  const add: string[] = [];
  for (const label of labels) if (!hub.includes(label) && !add.includes(label)) add.push(label);
  return { add, remove: hub.filter((l) => !labels.includes(l)) };
}

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
  if (after.add_labels !== undefined || after.remove_labels !== undefined) {
    const change = [...(after.add_labels ?? []).map((l) => `+ ${l}`), ...(after.remove_labels ?? []).map((l) => `− ${l}`)];
    line('labels', before.labels === undefined ? undefined : labels(before.labels), change.join(', '));
  }
  if (after.milestone !== undefined) line('milestone', before.milestone, after.milestone);
  if (after.epic !== undefined) line('epic', before.epic, after.epic);
  if (after.state !== undefined) {
    const reason = after.close_reason === 'completed' ? ' (completed)' : after.close_reason === 'not_planned' ? ' (not planned)' : '';
    line('state', before.state, `${after.state}${reason}`);
  }
  if (after.comment !== undefined) out.push(`comment: ${shown(after.comment)}`);
  return out;
}

const FIELD_ORDER = ['title', 'body', 'labels', 'add_labels', 'remove_labels', 'milestone', 'epic', 'state', 'close_reason', 'comment'] as const;

/** A field's name as the tracker calls it. */
function fieldName(field: string, system: string): string {
  if (field === 'body' && system === 'jira') return 'description';
  if (field === 'title' && system === 'jira') return 'summary';
  if (field === 'add_labels' || field === 'remove_labels') return 'labels';
  return field;
}

function askText(write: WriteProposal, taskKey: string, why: string, leftOut: string[]): { title: string; body: string } {
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
      const fields: string[] = [];
      for (const f of FIELD_ORDER) {
        if (write.after[f] === undefined) continue;
        const shownName = fieldName(f, write.system);
        if (!fields.includes(shownName)) fields.push(shownName);
      }
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
  if (leftOut.length > 0) {
    const names = leftOut.map((f) => fieldName(f, write.system));
    body += `\nNot sent: the ${names.join(' and the ')}. ${name}'s copy holds formatting or characters PitCrew does not keep, and sending PitCrew's would replace them; change it in ${name}.\n`;
  }
  body += `\n${why}`;
  if (write.target?.url !== undefined) body += `\n\n${write.target.url}`;
  return { title, body };
}

// ─── Proposing ──────────────────────────────────────────────────────────────────────────────────

function propose(
  hub: Hub,
  member: MemberId,
  to: MemberId,
  write: WriteProposal,
  taskKey: string,
  why: string,
  leftOut: string[] = [],
): UpstreamWrite | undefined {
  const s = state(hub);
  if (write.cause !== undefined && s.writes.some((w) => w.proposal.cause === write.cause)) return undefined;
  const { title, body } = askText(write, taskKey, why, leftOut);
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

interface Planned {
  operation: WriteProposal['operation'];
  before: WriteFields;
  after: WriteFields;
  leftOut: string[];
}

function planChange(
  hub: Hub,
  body: EventBody,
  integration: Integration,
  scope: string,
  seen: integrations.Item | undefined,
): Planned | undefined {
  const system = integration.settings.kind;
  const before: WriteFields = {};
  const after: WriteFields = {};
  const leftOut: string[] = [];
  if (body.type === 'task_moved') {
    const { from, to } = body.data;
    if (closed(from) === closed(to)) return undefined;
    const closing = closed(to);
    if (seen !== undefined && seen.open !== closing) return undefined;
    if (seen !== undefined) before.state = closing ? 'open' : 'closed';
    after.state = closing ? 'closed' : 'open';
    if (closing && system === 'github') after.close_reason = to === 'canceled' ? 'not_planned' : 'completed';
    return { operation: closing ? 'close' : 'reopen', before, after, leftOut };
  }
  if (body.type !== 'task_updated') return undefined;
  // An update is checked against upstream's values as last read: none read, nothing proposed.
  if (seen === undefined) return undefined;
  const patch = body.data.patch;
  if (patch.title !== undefined && seen.title !== patch.title) {
    if (seen.titleExact) {
      after.title = patch.title;
      before.title = seen.title;
    } else leftOut.push('title');
  }
  if (patch.description !== undefined && seen.body !== patch.description) {
    if (seen.bodyExact) {
      after.body = capped(patch.description, MAX_BODY_CHARS);
      before.body = seen.body;
    } else leftOut.push('body');
  }
  if (patch.labels !== undefined) {
    const { add, remove } = labelChange(seen.labels, patch.labels);
    if (add.length > 0 || remove.length > 0) {
      if (add.length > 0) after.add_labels = add;
      if (remove.length > 0) after.remove_labels = remove;
      before.labels = [...seen.labels];
    }
  }
  if (typeof patch.workstream === 'string' && writesParent(integration)) {
    const workstream = hub.findWorkstream(patch.workstream);
    const parent = workstream === undefined ? undefined : parentIn(workstream, system, scope);
    if (parent !== undefined && seen.parent !== parent) {
      setParent(after, system, parent);
      setParent(before, system, seen.parent);
    }
  }
  return Object.keys(after).length === 0 ? undefined : { operation: 'update', before, after, leftOut };
}

/** Proposes what the changes since the last pass imply. */
function plan(hub: Hub): void {
  const s = state(hub);
  if (s.cursor === undefined || s.cursor > hub.rev) {
    s.cursor = hub.rev;
    return;
  }
  const events = hub.eventsAfter(s.cursor);
  s.cursor = hub.rev;
  for (const event of events) {
    // A sync's own changes come from upstream: never sent back, whichever integration made them.
    if (integrations.isSyncMember(hub, event.author)) continue;
    const body = event.body;
    if (body.type !== 'task_moved' && body.type !== 'task_updated') continue;
    const task = hub.findTaskById(body.data.task);
    const source = task?.source;
    if (task === undefined || source === undefined) continue;
    const container = containerOf(source);
    const found = container === undefined ? undefined : integrations.integrationFor(hub, source.system, container);
    const member = found === undefined ? undefined : integrations.syncMemberOf(hub, found.integration.id);
    if (found === undefined || member === undefined) continue;
    const seen = integrations.upstreamIssue(hub, found.integration, source.key);
    const planned = planChange(hub, body, found.integration, found.container, seen);
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
      operation: planned.operation,
      before: planned.before,
      after: planned.after,
      requested_by: event.author,
      cause: event.id,
    };
    propose(hub, member, found.integration.added_by, write, task.key, why, planned.leftOut);
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
      if (after.labels !== undefined) return `${task.key} was proposed in an older form; nothing was sent.`;
      const changed = `${task.key} changed since; nothing was sent.`;
      if (after.title !== undefined && after.title !== task.title) return changed;
      if (after.body !== undefined && after.body !== capped(task.description, MAX_BODY_CHARS)) return changed;
      if ((after.add_labels ?? []).some((l) => !task.labels.includes(l))) return changed;
      if ((after.remove_labels ?? []).some((l) => task.labels.includes(l))) return changed;
      const parent = parentOf(after);
      if (parent !== undefined) {
        const workstream = task.workstream === undefined ? undefined : hub.findWorkstream(task.workstream);
        if (workstream === undefined || parentIn(workstream, write.system, write.scope) !== parent) return changed;
      }
      return undefined;
    }
  }
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
  return integrations.exchange(hub, method, url);
}

const noFixture = (url: string): string => `request to ${url.split('?')[0] ?? url} failed: no recorded fixture matches this request`;

type Read<T> = { ok: true; value: T } | { ok: false; message: string; status?: number };

/** A `GET`'s answer as JSON, or why there is none. */
function readJson(hub: Hub, system: string, url: string): Read<unknown> {
  const answer = send(hub, 'GET', url);
  if (answer === undefined) return { ok: false, message: noFixture(url) };
  if (answer.status < 200 || answer.status >= 300) {
    return { ok: false, message: refusalMessage(system, answer.status, answer.body), status: answer.status };
  }
  try {
    return { ok: true, value: JSON.parse(answer.body) as unknown };
  } catch {
    return { ok: false, message: `${tracker(system)}'s answer could not be read` };
  }
}

/** The daemon's `expected_web_origin`: GitHub's web host and port for an integration's API root. */
function webOrigin(apiBase: string | undefined): { host: string; port: string } {
  if (apiBase === undefined) return { host: 'github.com', port: '443' };
  try {
    const url = new URL(apiBase);
    return { host: url.hostname, port: url.port === '' ? (url.protocol === 'http:' ? '80' : '443') : url.port };
  } catch {
    return { host: apiBase, port: '443' };
  }
}

/** The daemon's `trusted_html_url`: an `https` link with no user info, on exactly the web origin. */
export function trustedLink(value: unknown, apiBase: string | undefined): string | undefined {
  if (typeof value !== 'string' || value.length > 2048 || /[\u0000-\u001f\u007f-\u009f]/u.test(value)) return undefined;
  let url: URL;
  try {
    url = new URL(value);
  } catch {
    return undefined;
  }
  const origin = webOrigin(apiBase);
  const port = url.port === '' ? '443' : url.port;
  if (url.protocol !== 'https:' || url.username !== '' || url.password !== '') return undefined;
  return url.hostname === origin.host && port === origin.port ? url.href : undefined;
}

/** An issue as upstream has it now (the daemon's `Now`). */
interface Now {
  title: string;
  body: string;
  bodyExact: boolean;
  labels: string[];
  parent: string | undefined;
  open: boolean;
  url: string | undefined;
}

/** Reads the issue `write` changes as upstream has it now: its fixture, then what writes changed. */
function readNow(hub: Hub, integration: Integration, write: WriteProposal): Read<Now> {
  const settings = integration.settings;
  const key = write.target?.key ?? '';
  let item: integrations.Item;
  if (settings.kind === 'github') {
    const url = `${settings.api_base ?? GITHUB_API}/repos/${write.scope}/issues/${key.split('#')[1] ?? ''}`;
    const read = readJson(hub, 'github', url);
    if (!read.ok) return read;
    const i = read.value;
    if (!isRecord(i) || typeof i['title'] !== 'string' || typeof i['state'] !== 'string') {
      return { ok: false, message: "GitHub's answer could not be read: no title" };
    }
    const number = isRecord(i['milestone']) ? i['milestone']['number'] : undefined;
    item = {
      key,
      url: trustedLink(i['html_url'], settings.api_base) ?? '',
      title: i['title'],
      body: typeof i['body'] === 'string' ? i['body'] : '',
      labels: Array.isArray(i['labels']) ? i['labels'].flatMap((l) => (isRecord(l) && typeof l['name'] === 'string' ? [l['name']] : [])) : [],
      open: i['state'] !== 'closed',
      titleExact: true,
      bodyExact: true,
    };
    if (typeof number === 'number') item.parent = `${write.scope}#milestone:${number}`;
  } else {
    let fields = 'summary%2Cdescription%2Cstatus%2Clabels%2Cparent%2Cissuetype%2Cupdated';
    if (settings.deployment === 'data_center' && settings.epic_link_field !== undefined) fields += `%2C${settings.epic_link_field}`;
    const url = `${settings.site}/rest/api/${settings.deployment === 'cloud' ? 3 : 2}/issue/${key}?fields=${fields}`;
    const read = readJson(hub, 'jira', url);
    if (!read.ok) return read;
    const f = isRecord(read.value) ? read.value['fields'] : undefined;
    if (!isRecord(f) || typeof f['summary'] !== 'string') return { ok: false, message: "Jira's answer has no issue fields." };
    const description = typeof f['description'] === 'string' ? f['description'] : integrations.adfText(f['description']).join('');
    const category = isRecord(f['status']) && isRecord(f['status']['statusCategory']) ? f['status']['statusCategory']['key'] : 'new';
    const epicField = settings.deployment === 'data_center' ? settings.epic_link_field : undefined;
    const parent = isRecord(f['parent']) ? f['parent']['key'] : epicField === undefined ? undefined : f[epicField];
    item = {
      key,
      url: `${settings.site}/browse/${key}`,
      title: f['summary'],
      body: description,
      labels: Array.isArray(f['labels']) ? f['labels'].filter((l): l is string => typeof l === 'string') : [],
      open: category !== 'done',
      titleExact: true,
      bodyExact: integrations.descriptionExact(f['description'], description),
    };
    if (typeof parent === 'string') item.parent = parent;
  }
  const now = integrations.withOverlay(hub, item);
  return {
    ok: true,
    value: { title: now.title, body: now.body, bodyExact: now.bodyExact, labels: now.labels, parent: now.parent, open: now.open, url: now.url === '' ? undefined : now.url },
  };
}

/**
 * What is left of `write` to send, given upstream `now` (the daemon's `reconcile`): a field upstream
 * already holds is dropped, one still as `before` is kept, any other is a change since (`changed`).
 */
function reconcile(write: WriteProposal, now: Now): { rest: WriteFields } | { changed: string[] } {
  const { before, after } = write;
  const jira = write.system === 'jira';
  const rest: WriteFields = {};
  const changed: string[] = [];
  if (after.title !== undefined && now.title !== after.title) {
    if (before.title === now.title) rest.title = after.title;
    else changed.push(jira ? 'summary' : 'title');
  }
  if (after.body !== undefined && now.body !== after.body) {
    if (now.bodyExact && before.body === now.body) rest.body = after.body;
    else changed.push(jira ? 'description' : 'body');
  }
  const parent = parentOf(after);
  if (parent !== undefined && now.parent !== parent) {
    if (now.parent === parentOf(before)) setParent(rest, write.system, parent);
    else changed.push(jira ? 'epic' : 'milestone');
  }
  if (after.state !== undefined && now.open !== (after.state === 'open')) {
    rest.state = after.state;
    if (after.close_reason !== undefined) rest.close_reason = after.close_reason;
  }
  if (after.add_labels !== undefined) {
    const missing = after.add_labels.filter((l) => !now.labels.some((r) => held(r) === l));
    if (missing.length > 0) rest.add_labels = missing;
  }
  const remove = after.remove_labels;
  if (remove !== undefined) {
    const present = now.labels.filter((r) => {
      const h = held(r);
      return h !== undefined && remove.includes(h);
    });
    if (present.length > 0) rest.remove_labels = present;
  }
  return changed.length > 0 ? { changed } : { rest };
}

/** The result of a write upstream answered with `parsed` (its created issue, its link). */
function sentResult(integration: Integration, write: WriteProposal, parsed: Record<string, unknown>, link?: string): WriteResult {
  const settings = integration.settings;
  if (settings.kind === 'github') {
    const htmlUrl = trustedLink(parsed['html_url'], settings.api_base) ?? link;
    if (write.operation === 'create_issue') {
      const number = parsed['number'];
      if (typeof number !== 'number' || number <= 0) {
        return { outcome: 'failed', message: "GitHub's answer could not be read: no issue number; a retry first looks upstream for it." };
      }
      const url = htmlUrl ?? (settings.api_base === undefined ? `https://github.com/${write.scope}/issues/${number}` : undefined);
      const key = `${write.scope}#${number}`;
      return url === undefined ? { outcome: 'sent', created: { system: 'github', key } } : { outcome: 'sent', created: { system: 'github', key, url }, url };
    }
    const url = htmlUrl ?? write.target?.url;
    return url === undefined ? { outcome: 'sent' } : { outcome: 'sent', url };
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

/** The daemon's `find_earlier`: an earlier create or comment, made since `sinceMs`, as its result. */
function findEarlier(hub: Hub, integration: Integration, write: WriteProposal, sinceMs: number): Read<WriteResult | undefined> {
  const settings = integration.settings;
  const after = write.after;
  const create = write.operation === 'create_issue';
  if (settings.kind === 'github') {
    const api = `${settings.api_base ?? GITHUB_API}/repos/${write.scope}`;
    const since = new Date(sinceMs).toISOString().replace(/\.\d{3}Z$/, 'Z');
    const after2 = since.replace(/:/g, '%3A');
    const url = create
      ? `${api}/issues?state=all&sort=created&direction=desc&per_page=100&since=${after2}`
      : `${api}/issues/${write.target?.key.split('#')[1] ?? ''}/comments?since=${after2}&per_page=100`;
    const read = readJson(hub, 'github', url);
    if (!read.ok) return read;
    if (!Array.isArray(read.value)) return { ok: false, message: "GitHub's answer could not be read: the answer is not a list" };
    const hit = read.value.find(
      (item) =>
        isRecord(item) &&
        typeof item['created_at'] === 'string' &&
        /^\d{4}-\d{2}-\d{2}T\d{2}:\d{2}:\d{2}Z$/.test(item['created_at']) &&
        item['created_at'] >= since &&
        (create
          ? item['pull_request'] === undefined && item['title'] === after.title && (typeof item['body'] === 'string' ? item['body'] : '') === (after.body ?? '')
          : item['body'] === after.comment),
    );
    if (!isRecord(hit)) return { ok: true, value: undefined };
    if (create && (typeof hit['number'] !== 'number' || hit['number'] <= 0)) return { ok: true, value: undefined };
    return { ok: true, value: sentResult(integration, write, create ? hit : { html_url: hit['html_url'] }) };
  }
  const api = `${settings.site}/rest/api/${settings.deployment === 'cloud' ? 3 : 2}`;
  const text = (t: string): unknown => (settings.deployment === 'cloud' ? integrations.adf(t) : t);
  const holds = (value: unknown, t: string): boolean =>
    value === undefined || value === null ? t.trim() === '' : JSON.stringify(value) === JSON.stringify(text(t));
  const createdSince = (value: unknown): boolean => {
    if (typeof value !== 'string') return false;
    const at = Date.parse(value.replace(/([+-]\d{2})(\d{2})$/, '$1:$2'));
    return Number.isFinite(at) && Math.floor(at / 1000) >= Math.floor(sinceMs / 1000);
  };
  if (create) {
    const jql = encodeURIComponent(`project = "${write.scope}" AND reporter = currentUser() ORDER BY created DESC`).replace(/\(/g, '%28').replace(/\)/g, '%29');
    const path = settings.deployment === 'cloud' ? '/search/jql' : '/search';
    const paging = settings.deployment === 'cloud' ? '' : '&startAt=0';
    const read = readJson(hub, 'jira', `${api}${path}?jql=${jql}${paging}&maxResults=50&fields=summary%2Cdescription%2Ccreated`);
    if (!read.ok) return read;
    const issues = isRecord(read.value) && Array.isArray(read.value['issues']) ? read.value['issues'] : [];
    const hit = issues.find((i) => {
      const f = isRecord(i) ? i['fields'] : undefined;
      return isRecord(f) && createdSince(f['created']) && f['summary'] === after.title && holds(f['description'], after.body ?? '');
    });
    return { ok: true, value: isRecord(hit) ? sentResult(integration, write, hit) : undefined };
  }
  const key = write.target?.key ?? '';
  const read = readJson(hub, 'jira', `${api}/issue/${key}/comment?orderBy=-created&maxResults=100`);
  if (!read.ok) return read;
  const comments = isRecord(read.value) && Array.isArray(read.value['comments']) ? read.value['comments'] : [];
  const hit = comments.some((c) => isRecord(c) && createdSince(c['created']) && holds(c['body'], after.comment ?? ''));
  return { ok: true, value: hit ? sentResult(integration, write, {}) : undefined };
}

/** Sends `fields` (what is left of `write`): each request once, stopping at the first refusal. */
function deliver(hub: Hub, integration: Integration, write: WriteProposal, fields: WriteFields): WriteResult {
  const settings = integration.settings;
  const requests: { method: string; url: string; body?: unknown }[] = [];
  if (settings.kind === 'github') {
    const api = `${settings.api_base ?? GITHUB_API}/repos/${write.scope}`;
    const issue = `${api}/issues/${write.target?.key.split('#')[1] ?? ''}`;
    const milestone = fields.milestone === undefined ? undefined : Number(fields.milestone.slice(fields.milestone.indexOf('#milestone:') + 11));
    if (write.operation === 'create_issue') {
      requests.push({ method: 'POST', url: `${api}/issues`, body: { title: fields.title ?? '', body: fields.body ?? '', labels: fields.labels ?? [], ...(milestone === undefined ? {} : { milestone }) } });
    } else if (write.operation === 'comment') {
      requests.push({ method: 'POST', url: `${issue}/comments`, body: { body: fields.comment ?? '' } });
    } else {
      const edit: Record<string, unknown> = {};
      if (fields.title !== undefined) edit['title'] = fields.title;
      if (fields.body !== undefined) edit['body'] = fields.body;
      if (milestone !== undefined) edit['milestone'] = milestone;
      if (fields.state !== undefined) edit['state'] = fields.state;
      if (fields.close_reason !== undefined) edit['state_reason'] = fields.close_reason;
      if (Object.keys(edit).length > 0) requests.push({ method: 'PATCH', url: issue, body: edit });
      if (fields.add_labels !== undefined) requests.push({ method: 'POST', url: `${issue}/labels`, body: { labels: fields.add_labels } });
      for (const label of fields.remove_labels ?? []) requests.push({ method: 'DELETE', url: `${issue}/labels/${encodeURIComponent(label)}` });
    }
  } else {
    const api = `${settings.site}/rest/api/${settings.deployment === 'cloud' ? 3 : 2}`;
    const text = (t: string): unknown => (settings.deployment === 'cloud' ? integrations.adf(t) : t);
    const epic = (out: Record<string, unknown>): void => {
      if (fields.epic === undefined) return;
      if (settings.deployment === 'cloud') out['parent'] = { key: fields.epic };
      else if (settings.epic_link_field !== undefined) out[settings.epic_link_field] = fields.epic;
    };
    const key = write.target?.key ?? '';
    if (write.operation === 'create_issue') {
      const out: Record<string, unknown> = { summary: fields.title ?? '', description: text(fields.body ?? ''), labels: fields.labels ?? [] };
      epic(out);
      out['project'] = { key: write.scope };
      out['issuetype'] = { name: 'Task' };
      requests.push({ method: 'POST', url: `${api}/issue`, body: { fields: out } });
    } else if (write.operation === 'comment') {
      requests.push({ method: 'POST', url: `${api}/issue/${key}/comment`, body: { body: text(fields.comment ?? '') } });
    } else if ((write.operation === 'close' || write.operation === 'reopen') && fields.state !== undefined) {
      const transitions = `${api}/issue/${key}/transitions`;
      const listed = send(hub, 'GET', transitions);
      if (listed === undefined) return { outcome: 'failed', message: noFixture(transitions) };
      if (listed.status < 200 || listed.status >= 300) {
        return { outcome: 'failed', message: refusalMessage('jira', listed.status, listed.body), status: listed.status };
      }
      const want = fields.state === 'open' ? 'new' : 'done';
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
      requests.push({ method: 'POST', url: transitions, body: { transition: { id } } });
    } else {
      const out: Record<string, unknown> = {};
      if (fields.title !== undefined) out['summary'] = fields.title;
      if (fields.body !== undefined) out['description'] = text(fields.body);
      epic(out);
      const labels = [...(fields.add_labels ?? []).map((l) => ({ add: l })), ...(fields.remove_labels ?? []).map((l) => ({ remove: l }))];
      const body: Record<string, unknown> = {};
      if (Object.keys(out).length > 0) body['fields'] = out;
      if (labels.length > 0) body['update'] = { labels };
      requests.push({ method: 'PUT', url: `${api}/issue/${key}`, body });
    }
  }
  let parsed: Record<string, unknown> | undefined;
  let link: string | undefined;
  for (const request of requests) {
    const answer = send(hub, request.method, request.url, request.body);
    if (answer === undefined) return { outcome: 'failed', message: noFixture(request.url) };
    if (request.method === 'DELETE' && answer.status === 404) continue;
    if (answer.status < 200 || answer.status >= 300) {
      return { outcome: 'failed', message: refusalMessage(settings.kind, answer.status, answer.body), status: answer.status };
    }
    try {
      const value: unknown = JSON.parse(answer.body);
      if (isRecord(value)) {
        parsed ??= value;
        if (settings.kind === 'github') link ??= trustedLink(value['html_url'], settings.api_base);
      }
    } catch {
      // An empty answer (204).
    }
  }
  return sentResult(integration, write, parsed ?? {}, link);
}

/** The member that proposed `write`: its approval ask's. */
function proposer(hub: Hub, write: UpstreamWrite): MemberId | undefined {
  return hub.findAsk(write.proposal.ask)?.from;
}

/**
 * hub-work's `start_write` refusal: an approved write starts only from its own integration's sync
 * member's approval ask, answered "Send" by a person; a failed one only with a person's retry
 * request no attempt has used.
 */
function startRefusal(hub: Hub, write: UpstreamWrite): string | undefined {
  if (write.state === 'failed') {
    const by = write.retry_requested_by === undefined ? undefined : hub.findMember(write.retry_requested_by);
    return by?.kind === 'human' ? undefined : 'No person asked to send it again.';
  }
  if (write.state !== 'approved') return `This write is ${write.state}.`;
  const ask = hub.findAsk(write.proposal.ask);
  if (ask === undefined || ask.kind !== 'approval' || ask.from !== integrations.syncMemberOf(hub, write.proposal.integration)) {
    return "Its ask is not the sync's approval ask.";
  }
  if (ask.answer?.option !== 0) return 'Its approval ask was not answered "Send".';
  return hub.findMember(ask.answer.by)?.kind === 'human' ? undefined : 'Its approval ask was not answered by a person.';
}

function finish(hub: Hub, write: UpstreamWrite, result: WriteResult): void {
  const member = proposer(hub, write);
  if (member === undefined) return;
  const event = hub.append(member, {
    type: 'write_finished',
    data: write.proposal.task === undefined ? { ask: write.proposal.ask, result } : { ask: write.proposal.ask, task: write.proposal.task, result },
  });
  write.state = result.outcome;
  write.result = result;
  write.finished_at = event.at;
}

function start(hub: Hub, write: UpstreamWrite, member: MemberId): void {
  write.state = 'sending';
  write.attempts += 1;
  delete write.retry_requested_by;
  hub.append(member, {
    type: 'write_started',
    data: write.proposal.task === undefined ? { ask: write.proposal.ask, attempt: write.attempts } : { ask: write.proposal.ask, task: write.proposal.task, attempt: write.attempts },
  });
}

/** A failure from a read: `prefix`, then its message, with upstream's status when it answered. */
function failedRead(prefix: string, read: { message: string; status?: number }): WriteResult {
  const failed: WriteResult = { outcome: 'failed', message: `${prefix}: ${read.message}` };
  return read.status === undefined ? failed : { ...failed, status: read.status };
}

function sendWrite(hub: Hub, write: UpstreamWrite): void {
  const member = proposer(hub, write);
  if (member === undefined) return;
  const stale = staleReason(hub, write.proposal);
  const integration = integrations.integrationById(hub, write.proposal.integration);
  if (stale !== undefined || integration === undefined) {
    finish(hub, write, { outcome: 'not_sent', reason: stale ?? 'Its integration was removed; nothing was sent.' });
    return;
  }
  if (startRefusal(hub, write) !== undefined) return;
  const p = write.proposal;
  const key = p.target?.key ?? 'The issue';
  let fields: WriteFields | undefined;
  let now: Now | undefined;
  let result: WriteResult | undefined;
  const missing = integrations.missingCredential(hub, integration.id);
  if (missing !== undefined) {
    result = { outcome: 'failed', message: missing };
  } else if (p.operation === 'create_issue' || p.operation === 'comment') {
    if (write.attempts > 0) {
      const earlier = findEarlier(hub, integration, p, (write.answered_at ?? write.proposed_at) - EARLIER_MARGIN_MS);
      if (!earlier.ok) result = failedRead('Could not look upstream for the earlier attempt, so nothing was sent', earlier);
      else if (earlier.value !== undefined) result = earlier.value;
      else fields = p.after;
    } else fields = p.after;
  } else {
    const read = readNow(hub, integration, p);
    if (!read.ok) {
      result = failedRead(`Could not read ${key} before sending, so nothing was sent`, read);
    } else {
      now = read.value;
      const reconciled = reconcile(p, now);
      if ('changed' in reconciled) {
        // Nothing was started, and nothing is sent.
        finish(hub, write, {
          outcome: 'not_sent',
          reason: `Not sent: ${key} changed upstream since this was proposed (${reconciled.changed.join(', ')}). The next sync brings that change into PitCrew.`,
        });
        return;
      }
      if (Object.keys(reconciled.rest).length === 0) {
        const url = now.url ?? p.target?.url;
        result = url === undefined ? { outcome: 'sent' } : { outcome: 'sent', url };
      } else fields = reconciled.rest;
    }
  }
  start(hub, write, member);
  result ??= deliver(hub, integration, p, fields ?? {});
  if (result.outcome === 'sent') {
    const task = p.task === undefined ? undefined : hub.findTaskById(p.task);
    if (result.created !== undefined && task !== undefined && task.source === undefined) task.source = result.created;
    if (p.target !== undefined && fields !== undefined) {
      const change: integrations.Overlay = {};
      if (fields.title !== undefined) change.title = fields.title;
      if (fields.body !== undefined) change.body = fields.body;
      if ((fields.add_labels !== undefined || fields.remove_labels !== undefined) && now !== undefined) {
        const removed = new Set(fields.remove_labels ?? []);
        change.labels = [...now.labels.filter((l) => !removed.has(l)), ...(fields.add_labels ?? [])];
      }
      const parent = parentOf(fields);
      if (parent !== undefined) change.parent = parent;
      if (fields.state !== undefined) change.open = fields.state === 'open';
      integrations.changeUpstream(hub, p.target.key, change);
    }
  }
  finish(hub, write, result);
}

/** One pass: propose, then act on answers and retry requests. */
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
    if (write.state === 'sending') {
      finish(hub, write, { outcome: 'failed', message: CUT_OFF });
    } else if (write.state === 'denied') {
      const name = write.answered_by === undefined ? 'the person' : (hub.findMember(write.answered_by)?.name ?? 'the person');
      finish(hub, write, { outcome: 'not_sent', reason: `Not sent: ${name} chose not to.` });
    } else if (write.state === 'approved' || (write.state === 'failed' && write.retry_requested_by !== undefined)) {
      sendWrite(hub, write);
    }
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

/** `POST /v1/writes/{id}/retry`: records the caller's request (`write_retry_requested`), once. */
export function retry(hub: Hub, caller: MemberId, id: string): Reply {
  const write = writeAt(hub, id);
  const ask = hub.findAsk(write.proposal.ask);
  if (ask === undefined || ask.to !== caller) {
    throw forbidden('A person may only answer asks addressed to them or to their agents.');
  }
  if (write.state !== 'failed') {
    throw conflict(`Only a failed write is sent again; this one is ${write.state}.`);
  }
  if (write.retry_requested_by === undefined) {
    hub.append(caller, {
      type: 'write_retry_requested',
      data: write.proposal.task === undefined ? { ask: write.proposal.ask, by: caller } : { ask: write.proposal.ask, task: write.proposal.task, by: caller },
    });
    write.retry_requested_by = caller;
  }
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
  const member = integrations.syncMemberOf(hub, write.integration);
  if (member === undefined) throw conflict("The integration's sync member cannot be found.");
  const handle = hub.findMember(caller)?.handle ?? 'a person';
  const proposed = propose(hub, member, to, write, task.key, `Asked for by ${handle}.`);
  if (proposed === undefined) throw new Error('a request has no cause, so it is never a duplicate');
  return { status: 201, body: proposed };
}
