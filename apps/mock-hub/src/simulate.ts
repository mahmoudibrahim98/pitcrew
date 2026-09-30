// Simulated liveness. A real runner reports sessions starting, answering and ending; the mock does
// the same on timers, so the UI sees the events it will see from the daemon.

import { randomUUID } from 'node:crypto';
import { canMove } from './rules.ts';
import type { Hub } from './state.ts';
import {
  appendRecord,
  assistantText,
  turnEnded,
  userPrompt,
  type TranscriptRecord,
} from './transcripts.ts';
import type {
  Ask,
  EndMode,
  Engine,
  LinkBasis,
  MachineId,
  MemberId,
  Mover,
  Session,
  SessionState,
  TaskId,
  WorkstreamId,
} from './types.ts';
import { ulid } from './ulid.ts';

/** What a dispatch or a start request asks for. */
export interface SessionDraft {
  engine: Engine;
  machine: MachineId;
  cwd: string;
  branch?: string | undefined;
  title?: string | undefined;
  agent?: MemberId | undefined;
  workstream?: WorkstreamId | undefined;
  task?: TaskId | undefined;
  link_basis?: LinkBasis | undefined;
  /** The first prompt, recorded as the transcript's first item. */
  brief?: string | undefined;
}

/** Creates a session in state `starting`, with a terminal and a transcript. Emits nothing. */
export function createSession(hub: Hub, draft: SessionDraft): Session {
  const now = Date.now();
  const session: Session = {
    id: ulid(),
    engine: draft.engine,
    native_id: randomUUID(),
    machine: draft.machine,
    cwd: draft.cwd,
    branch: draft.branch,
    title: draft.title,
    agent: draft.agent,
    workstream: draft.workstream,
    task: draft.task,
    link_basis: draft.link_basis,
    state: 'starting',
    started: now,
    last_activity: now,
    terminal: ulid(),
  };
  hub.sessions.push(session);
  const records: TranscriptRecord[] = [];
  if (draft.brief !== undefined && draft.brief !== '') {
    appendRecord(records, [userPrompt(now, draft.brief)]);
  }
  hub.transcripts.set(session.id, records);
  return session;
}

/**
 * Emits `session_discovered`. After the start delay the session turns `working`, and a
 * dispatched agent moves its task to in progress, as the agents in the fixture do.
 */
export function announceSession(hub: Hub, session: Session): void {
  hub.append(authorOf(hub, session), { type: 'session_discovered', data: { session } });
  hub.later(hub.delays.start, () => {
    if (session.state === 'starting') {
      setSessionState(hub, session, 'working', 'Reading the brief');
      claimTask(hub, session);
    }
  });
}

/** Types `text` into the session. A canned reply and `turn_ended` follow after the reply delay. */
export function sendText(hub: Hub, session: Session, text: string): void {
  const now = Date.now();
  appendRecord(transcriptOf(hub, session), [userPrompt(now, text)]);
  session.last_activity = now;
  startTurn(hub, session, 'Thinking', cannedReply(text));
}

/** Stops the current turn: its pending reply is dropped. */
export function interrupt(hub: Hub, session: Session): void {
  if (session.state === 'working') {
    nextTurn(hub, session);
    setSessionState(hub, session, 'waiting', 'Interrupted');
  }
}

/** Ends the session: at once when killed, after the end delay when graceful. */
export function endSession(hub: Hub, session: Session, mode: EndMode): void {
  if (mode === 'kill') {
    finishEnd(hub, session);
  } else {
    hub.later(hub.delays.end, () => finishEnd(hub, session));
  }
}

/** A session waiting on an answered ask carries on, and ends its turn after the reply delay. */
export function resumeAfterAnswer(hub: Hub, session: Session): void {
  if (session.state === 'waiting') {
    startTurn(hub, session, 'Continuing after your answer', 'Thanks. Carrying on with that.');
  }
}

/** A session whose agent raised an ask waits for the answer. */
export function waitOnAsk(hub: Hub, session: Session, ask: Ask): void {
  if (session.state === 'working' || session.state === 'idle') {
    nextTurn(hub, session);
    setSessionState(hub, session, 'waiting', `Asks: ${ask.title}`);
  }
}

/** Changes the session's state and emits `session_state_changed`. */
export function setSessionState(
  hub: Hub,
  session: Session,
  to: SessionState,
  statusLine?: string,
): void {
  const from = session.state;
  if (from === to) {
    return;
  }
  session.state = to;
  session.status_line = statusLine;
  session.last_activity = Date.now();
  const data =
    statusLine === undefined
      ? { session: session.id, from, to }
      : { session: session.id, from, to, status_line: statusLine };
  hub.append(authorOf(hub, session), { type: 'session_state_changed', data });
}

// ─── Internals ──────────────────────────────────────────────────────────────────────────────────

function startTurn(hub: Hub, session: Session, statusLine: string, reply: string): void {
  const turn = nextTurn(hub, session);
  setSessionState(hub, session, 'working', statusLine);
  hub.later(hub.delays.reply, () => finishTurn(hub, session, turn, reply));
}

function finishTurn(hub: Hub, session: Session, turn: number, reply: string): void {
  if (hub.turns.get(session.id) !== turn || session.state === 'ended') {
    return;
  }
  const now = Date.now();
  const records = transcriptOf(hub, session);
  appendRecord(records, [assistantText(now, reply)]);
  const end = appendRecord(records, [turnEnded(now)]);
  hub.append(authorOf(hub, session), {
    type: 'turn_ended',
    data: { session: session.id, receipt: { kind: 'transcript', session: session.id, offset: end.offset } },
  });
  setSessionState(hub, session, 'waiting', 'Turn ended');
}

function finishEnd(hub: Hub, session: Session): void {
  if (session.state === 'ended') {
    return;
  }
  nextTurn(hub, session);
  session.state = 'ended';
  delete session.status_line;
  session.last_activity = Date.now();
  hub.append(authorOf(hub, session), { type: 'session_ended', data: { session: session.id } });
}

function claimTask(hub: Hub, session: Session): void {
  if (session.agent === undefined || session.task === undefined) {
    return;
  }
  const task = hub.findTaskById(session.task);
  if (task === undefined) {
    return;
  }
  const mover: Mover = { kind: 'agent', on_own_task: hub.isOwnTask(task, session.agent) };
  if (!canMove(task.status, 'in_progress', mover)) {
    return;
  }
  const from = task.status;
  task.status = 'in_progress';
  hub.append(session.agent, {
    type: 'task_moved',
    data: { task: task.id, from, to: 'in_progress', mover },
  });
}

/** Runner events are authored by the session's agent, or by the person for unnamed runs. */
function authorOf(hub: Hub, session: Session): MemberId {
  return session.agent ?? hub.person;
}

function transcriptOf(hub: Hub, session: Session): TranscriptRecord[] {
  let records = hub.transcripts.get(session.id);
  if (records === undefined) {
    records = [];
    hub.transcripts.set(session.id, records);
  }
  return records;
}

function nextTurn(hub: Hub, session: Session): number {
  const turn = (hub.turns.get(session.id) ?? 0) + 1;
  hub.turns.set(session.id, turn);
  return turn;
}

function cannedReply(prompt: string): string {
  const oneLine = prompt.replace(/\s+/g, ' ').trim();
  const preview = oneLine.length > 60 ? `${oneLine.slice(0, 57)}...` : oneLine;
  return `Mock reply to "${preview}". The mock hub runs no agent; this reply is canned.`;
}
