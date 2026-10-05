// Labels, tones and sentences for the projects layout. Pure functions, no React.

import type { Tone } from '../design/index.ts';
import type {
  AskKind,
  Block,
  CalendarDate,
  Event,
  Health,
  Priority,
  ProjectStatus,
  Receipt,
  SessionState,
  TaskStatus,
  TimestampMs,
  WorkstreamStatus,
  WriteOperation,
  WriteState,
} from '../data/index.ts';

type Labelled = { label: string; tone: Tone };

/** Board columns, left to right. Canceled shows only when a task is in it. */
export const STATUS_ORDER: readonly TaskStatus[] = ['backlog', 'todo', 'in_progress', 'review', 'done', 'canceled'];

export const TASK_STATUS: Record<TaskStatus, Labelled> = {
  backlog: { label: 'Backlog', tone: 'neutral' },
  todo: { label: 'Todo', tone: 'neutral' },
  in_progress: { label: 'In progress', tone: 'progress' },
  review: { label: 'Review', tone: 'accent' },
  done: { label: 'Done', tone: 'ok' },
  canceled: { label: 'Canceled', tone: 'neutral' },
};

export const PRIORITY: Record<Priority, Labelled & { rank: number }> = {
  urgent: { label: 'Urgent', tone: 'risk', rank: 0 },
  high: { label: 'High', tone: 'warn', rank: 1 },
  medium: { label: 'Medium', tone: 'neutral', rank: 2 },
  low: { label: 'Low', tone: 'neutral', rank: 3 },
  none: { label: 'No priority', tone: 'neutral', rank: 4 },
};

export const SESSION_STATE: Record<SessionState, Labelled & { rank: number }> = {
  waiting: { label: 'Waiting', tone: 'warn', rank: 0 },
  working: { label: 'Working', tone: 'progress', rank: 1 },
  starting: { label: 'Starting', tone: 'accent', rank: 2 },
  idle: { label: 'Idle', tone: 'neutral', rank: 3 },
  unreachable: { label: 'Unreachable', tone: 'risk', rank: 4 },
  ended: { label: 'Ended', tone: 'neutral', rank: 5 },
};

export const HEALTH: Record<Health, Labelled> = {
  on_track: { label: 'On track', tone: 'ok' },
  at_risk: { label: 'At risk', tone: 'warn' },
  blocked: { label: 'Blocked', tone: 'risk' },
};

export const WORKSTREAM_STATUS: Record<WorkstreamStatus, Labelled> = {
  idea: { label: 'Idea', tone: 'neutral' },
  active: { label: 'Active', tone: 'accent' },
  paused: { label: 'Paused', tone: 'warn' },
  shipped: { label: 'Shipped', tone: 'ok' },
  dropped: { label: 'Dropped', tone: 'neutral' },
};

export const PROJECT_STATUS: Record<ProjectStatus, Labelled> = {
  planning: { label: 'Planning', tone: 'neutral' },
  in_progress: { label: 'In progress', tone: 'progress' },
  on_hold: { label: 'On hold', tone: 'warn' },
  completed: { label: 'Completed', tone: 'ok' },
};

/** Inbox groups, in order. */
export const ASK_KINDS: readonly AskKind[] = ['question', 'decision', 'review', 'approval', 'mention'];

export const ASK_KIND: Record<AskKind, { one: string; many: string }> = {
  question: { one: 'Question', many: 'Questions' },
  decision: { one: 'Decision', many: 'Decisions' },
  review: { one: 'Review', many: 'Reviews' },
  approval: { one: 'Approval', many: 'Approvals' },
  mention: { one: 'Mention', many: 'Mentions' },
};

const DAY = new Intl.DateTimeFormat(undefined, { day: 'numeric', month: 'short', timeZone: 'UTC' });
const WHEN = new Intl.DateTimeFormat(undefined, {
  day: 'numeric',
  month: 'short',
  hour: '2-digit',
  minute: '2-digit',
});

/** `2026-10-10` → `10 Oct`. Calendar dates have no time zone, so they are read as UTC. */
export function formatDay(date: CalendarDate): string {
  const parsed = Date.parse(`${date}T00:00:00Z`);
  return Number.isNaN(parsed) ? date : DAY.format(parsed);
}

const LONG_DAY = new Intl.DateTimeFormat(undefined, {
  weekday: 'long',
  day: 'numeric',
  month: 'long',
  year: 'numeric',
  timeZone: 'UTC',
});

/** `2026-09-30` → `Wednesday, 30 September 2026` (in the viewer's locale), read as UTC like `formatDay`. */
export function formatLongDay(date: CalendarDate): string {
  const parsed = Date.parse(`${date}T00:00:00Z`);
  return Number.isNaN(parsed) ? date : LONG_DAY.format(parsed);
}

export function formatWhen(at: TimestampMs): string {
  return WHEN.format(at);
}

const TIME = new Intl.DateTimeFormat(undefined, { hour: '2-digit', minute: '2-digit' });
const DATE_KEY = new Intl.DateTimeFormat('en-CA', { year: 'numeric', month: '2-digit', day: '2-digit' });

/** A stretch of time: `30 Sep, 08:00–08:15`; one time when it starts and ends in the same minute. */
export function formatSpan(start: TimestampMs, end: TimestampMs): string {
  const from = formatWhen(start);
  if (Math.floor(start / 60_000) === Math.floor(end / 60_000)) return from;
  return DATE_KEY.format(start) === DATE_KEY.format(end) ? `${from}–${TIME.format(end)}` : `${from} – ${formatWhen(end)}`;
}

const plural = (n: number, one: string, many: string): string => `${n} ${n === 1 ? one : many}`;

/**
 * What a block of work counted, as short phrases: files touched (with the lines added and
 * removed), tools run and failed, turns. Zero counts are left out; a block with none of these says
 * how many events it covers instead.
 */
export function describeCounts(block: Block): string[] {
  const { counts } = block;
  const files = block.files.length + block.files_omitted;
  const out: string[] = [];
  if (files > 0) {
    out.push(`${plural(files, 'file', 'files')} touched (+${counts.lines_added} −${counts.lines_removed})`);
  }
  if (counts.tools_run > 0) {
    const run = plural(counts.tools_run, 'tool run', 'tools run');
    out.push(counts.tools_failed > 0 ? `${run}, ${counts.tools_failed} failed` : run);
  }
  if (counts.turns > 0) out.push(plural(counts.turns, 'turn', 'turns'));
  if (out.length === 0) out.push(plural(counts.events, 'event', 'events'));
  return out;
}

export function initials(name: string): string {
  const words = name.split(/[\s·]+/).filter((w) => /\p{L}|\p{N}/u.test(w));
  const letters = words.slice(0, 2).map((w) => Array.from(w)[0] ?? '');
  return letters.join('').toUpperCase() || '?';
}

/** Names the lookups know; anything unknown falls back to a short id. */
export interface Names {
  member(id: string): string;
  task(id: string): string;
  workstream(id: string): string;
  project(id: string): string;
  session(id: string): string;
  machine(id: string): string;
  ask(id: string): string | undefined;
}

export const shortId = (id: string): string => `…${id.slice(-4)}`;

export const plainNames: Names = {
  member: shortId,
  task: shortId,
  workstream: shortId,
  project: shortId,
  session: shortId,
  machine: shortId,
  ask: () => undefined,
};

/** What an event did, as the rest of a sentence whose subject is its author. */
export function describeEvent(event: Event, names: Names): string {
  const { body } = event;
  switch (body.type) {
    case 'safety_changed':
      return 'updated workspace safety settings';
    case 'cursor_moved':
      return 'marked changes as read';
    case 'machine_added':
      return `added the machine ${body.data.machine.name}`;
    case 'member_added':
      return body.data.member.kind === 'agent'
        ? `added the agent ${body.data.member.handle}`
        : `added ${body.data.member.handle}`;
    case 'persona_saved':
      return `saved the persona ${body.data.persona.name}`;
    case 'team_saved':
      return `saved the team ${body.data.team.name}`;
    case 'session_updated': {
      const changes = [
        body.data.title === undefined ? '' : `renamed it “${body.data.title}”`,
        body.data.branch === undefined ? '' : `on branch ${body.data.branch}`,
      ].filter(Boolean);
      return `updated “${names.session(body.data.session)}”${changes.length === 0 ? '' : `: ${changes.join(', ')}`}`;
    }
    case 'machine_liveness':
      return `saw ${names.machine(body.data.machine)} become ${body.data.liveness}`;
    case 'session_discovered':
      return `started the session “${body.data.session.title ?? shortId(body.data.session.id)}”`;
    case 'session_state_changed':
      return `${SESSION_STATE[body.data.to].label.toLowerCase()} in “${names.session(body.data.session)}”${
        body.data.status_line === undefined ? '' : `: ${body.data.status_line}`
      }`;
    case 'turn_ended':
      return `finished a turn in “${names.session(body.data.session)}”`;
    case 'tool_ran':
      return `ran ${body.data.tool} ${body.data.target}${body.data.failed ? ' (failed)' : ''}: ${body.data.outcome}`;
    case 'file_edited':
      return `edited ${body.data.path} (+${body.data.added} −${body.data.removed})`;
    case 'session_linked': {
      const to = body.data.task ?? body.data.workstream;
      const name = body.data.task !== undefined ? names.task(body.data.task) : names.workstream(to ?? '');
      return `linked “${names.session(body.data.session)}” to ${name}`;
    }
    case 'session_ended':
      return `ended “${names.session(body.data.session)}”`;
    case 'project_created':
      return `created the project ${body.data.project.name}`;
    case 'workstream_created':
      return `created the workstream ${body.data.workstream.name}`;
    case 'workstream_changed':
      return `marked ${names.workstream(body.data.workstream)} ${WORKSTREAM_STATUS[body.data.status].label.toLowerCase()}, ${HEALTH[body.data.health].label.toLowerCase()}`;
    case 'workstream_linked':
      return body.data.external.length === 0
        ? `unlinked ${names.workstream(body.data.workstream)} from upstream`
        : `linked ${names.workstream(body.data.workstream)} to ${body.data.external.map((l) => l.key).join(', ')}`;
    case 'task_created':
      return `created ${body.data.task.key} “${body.data.task.title}”`;
    case 'task_moved':
      return `moved ${names.task(body.data.task)} from ${TASK_STATUS[body.data.from].label} to ${TASK_STATUS[body.data.to].label}`;
    case 'task_updated':
      return `edited ${names.task(body.data.task)}`;
    case 'task_assigned':
      return body.data.assignee === undefined
        ? `unassigned ${names.task(body.data.task)}`
        : `assigned ${names.task(body.data.task)} to ${names.member(body.data.assignee)}`;
    case 'subtasks_replaced':
      return `updated the subtasks of ${names.task(body.data.task)}`;
    case 'dispatch_started':
      return `dispatched ${names.member(body.data.dispatch.agent)} to ${names.task(body.data.dispatch.task)}`;
    case 'dispatch_finished':
      return `finished a dispatch (${body.data.outcome})${body.data.summary === undefined ? '' : `: ${body.data.summary}`}`;
    case 'ask_raised':
      return `asked ${names.member(body.data.ask.to)}: ${body.data.ask.title}`;
    case 'ask_answered': {
      const title = names.ask(body.data.ask);
      return title === undefined ? 'answered an ask' : `answered “${title}”`;
    }
    case 'comment_posted':
      return `commented${body.data.task === undefined ? '' : ` on ${names.task(body.data.task)}`}: ${body.data.text}`;
    case 'brief_proposed':
      return `proposed where ${targetName(body.data.target, names)} stands`;
    case 'brief_accepted':
      return `${body.data.pinned ? 'pinned' : 'updated'} where ${targetName(body.data.target, names)} stands`;
    case 'decision_recorded':
      return `recorded a decision: ${body.data.text}`;
    case 'write_proposed': {
      const w = body.data.write;
      return `asked to ${WRITE_OPERATION[w.operation]} ${w.target?.key ?? w.scope} on ${tracker(w.system)}`;
    }
    case 'write_started':
      return `sent a write${body.data.task === undefined ? '' : ` for ${names.task(body.data.task)}`} upstream${body.data.attempt > 1 ? ` (attempt ${body.data.attempt})` : ''}`;
    case 'write_retry_requested':
      return `asked to send a failed write${body.data.task === undefined ? '' : ` for ${names.task(body.data.task)}`} again`;
    case 'write_finished': {
      const r = body.data.result;
      const about = body.data.task === undefined ? '' : ` for ${names.task(body.data.task)}`;
      if (r.outcome === 'sent') return `wrote upstream${about}${r.created === undefined ? '' : `: created ${r.created.key}`}`;
      if (r.outcome === 'failed') return `could not write upstream${about}: ${r.message}`;
      return `did not write upstream${about}: ${r.reason}`;
    }
  }
}

/** The tracker's name for people. */
export function tracker(system: string): string {
  return system === 'jira' ? 'Jira' : 'GitHub';
}

/** What an outward write does, as a verb phrase ("close", "create an issue in"). */
export const WRITE_OPERATION: Record<WriteOperation, string> = {
  create_issue: 'create an issue in',
  comment: 'comment on',
  update: 'change',
  close: 'close',
  reopen: 'reopen',
};

/** Where an outward write stands, for people, with a pill tone. */
export const WRITE_STATE: Record<WriteState, Labelled> = {
  pending: { label: 'Waiting for approval', tone: 'accent' },
  approved: { label: 'Approved', tone: 'progress' },
  denied: { label: 'Not approved', tone: 'neutral' },
  sending: { label: 'Sending', tone: 'progress' },
  sent: { label: 'Sent', tone: 'ok' },
  failed: { label: 'Failed', tone: 'risk' },
  not_sent: { label: 'Not sent', tone: 'neutral' },
};

function targetName(target: { kind: 'project' | 'workstream'; id: string }, names: Names): string {
  return target.kind === 'project' ? names.project(target.id) : names.workstream(target.id);
}

/** A receipt as a short label, and a longer title for a tooltip. */
export function describeReceipt(receipt: Receipt, names: Names): { label: string; title: string } {
  switch (receipt.kind) {
    case 'transcript':
      return {
        label: `Transcript @${receipt.offset}`,
        title: `“${names.session(receipt.session)}” at byte ${receipt.offset}`,
      };
    case 'commit':
      return { label: `Commit ${receipt.sha.slice(0, 7)}`, title: `${receipt.repo} @ ${receipt.sha}` };
    case 'pull_request':
      return { label: `Pull request ${receipt.url.split('/').slice(-1)[0] ?? ''}`.trim(), title: receipt.url };
    case 'job':
      return { label: `Job ${receipt.id}`, title: `${receipt.scheduler.toUpperCase()} job ${receipt.id}` };
    case 'file': {
      const name = receipt.location.path.split('/').filter(Boolean).at(-1) ?? receipt.location.path;
      return {
        label: `File ${name}`,
        title: `${receipt.location.path} on ${names.machine(receipt.location.machine)}`,
      };
    }
    case 'event':
      return { label: `Event ${shortId(receipt.id)}`, title: `Event ${receipt.id}` };
  }
}

/** Only web links become anchors. */
export function isWebUrl(url: string): boolean {
  return /^https?:\/\//i.test(url);
}
