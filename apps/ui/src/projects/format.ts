// Labels, tones and sentences for the projects layout. Pure functions, no React.

import type { Tone } from '../design/index.ts';
import type {
  AskKind,
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

export function formatWhen(at: TimestampMs): string {
  return WHEN.format(at);
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
    case 'task_created':
      return `created ${body.data.task.key} “${body.data.task.title}”`;
    case 'task_moved':
      return `moved ${names.task(body.data.task)} from ${TASK_STATUS[body.data.from].label} to ${TASK_STATUS[body.data.to].label}`;
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
  }
}

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
