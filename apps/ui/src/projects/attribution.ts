// Who did what, for the activity feed and "Since you last looked" (api-v1.md, "Sessions": "Who did
// it" and "Order"). The runner's session events carry the person's stamp, but they are the
// session's doing: named by the agent running it, or by the session itself; a sub-agent's are its
// parent's. Pure: the components pass in the sessions and names they already have.

import {
  aboutSubagent,
  actorName,
  actorOf,
  byTimeNewestFirst,
  eventSession,
  isNested,
  rootOf,
  sessionName,
  startedByPerson,
  type Actor,
  type Event,
  type MemberId,
  type Session,
  type SessionId,
} from '../data/index.ts';
import { describeEvent, SESSION_STATE, type Names } from './format.ts';

export interface Attributed {
  event: Event;
  rev: number;
  actor: Actor;
  /** The actor's name, as the line's subject. */
  who: string;
  /** The rest of the sentence. */
  what: string;
}

/** `event`'s doer and what it did, as one sentence: `${who} ${what}`. */
export function attribute(
  event: Event,
  rev: number,
  sessions: ReadonlyMap<SessionId, Session>,
  names: Names,
): Attributed {
  const actor = actorOf(event, sessions);
  return { event, rev, actor, who: actorName(actor, names.member), what: describeAs(event, actor, sessions, names) };
}

/**
 * What the event did, with `actor` as the subject. A session that is its own subject is not named
 * twice ("Claude · Fix the parser finished a turn"); a sub-agent's event says so.
 */
export function describeAs(
  event: Event,
  actor: Actor,
  sessions: ReadonlyMap<SessionId, Session>,
  names: Names,
): string {
  const { body } = event;
  const id = eventSession(event);
  const session = id === undefined ? undefined : (sessions.get(id) ?? (body.type === 'session_discovered' ? body.data.session : undefined));
  if (session === undefined) return describeEvent(event, names);
  const nested = isNested(session, sessions);
  // The session the sentence would otherwise name is its subject already.
  const self = actor.kind === 'session' && actor.session.id === rootOf(session, sessions).id;
  const where = self ? '' : ` in “${sessionName(rootOf(session, sessions))}”`;
  const sub = nested ? `a sub-agent “${sessionName(session)}”` : undefined;
  switch (body.type) {
    case 'session_discovered': {
      if (nested) return `started ${sub}${where}`;
      if (actor.kind === 'session') return `was found on ${names.machine(body.data.session.machine)}`;
      return `started the session “${sessionName(body.data.session)}”`;
    }
    case 'turn_ended':
      return nested ? `finished a turn in ${sub}` : `finished a turn${where}`;
    case 'session_state_changed': {
      const state = SESSION_STATE[body.data.to].label.toLowerCase();
      const line = body.data.status_line === undefined ? '' : `: ${body.data.status_line}`;
      return nested ? `${sub} is ${state}${line}` : `is ${state}${where}${line}`;
    }
    case 'session_ended':
      return nested ? `${sub} ended` : self ? 'ended' : `ended “${sessionName(session)}”`;
    case 'tool_ran':
    case 'file_edited':
      return nested ? `${describeEvent(event, names)} (in ${sub})` : describeEvent(event, names);
    default:
      return describeEvent(event, names);
  }
}

/** Activity, newest first by when it happened, each event attributed. */
export function attributedFeed(
  items: readonly { event: Event; rev: number }[],
  sessions: ReadonlyMap<SessionId, Session>,
  names: Names,
): Attributed[] {
  return byTimeNewestFirst(items).map(({ event, rev }) => attribute(event, rev, sessions, names));
}

/** One line of "Since you last looked". */
export interface ChangeLine {
  key: string;
  /** The newest event folded in. */
  event: Event;
  /** Every revision folded in. */
  revs: number[];
  who: string;
  what: string;
  /** Who did it, for the avatar. */
  actor: Actor;
}

interface SessionTally {
  root: Session;
  actor: Actor;
  newest: Event;
  revs: number[];
  found: boolean;
  started: boolean;
  turns: number;
  tools: number;
  edits: number;
  subagents: number;
  ended: boolean;
}

const plural = (n: number, one: string, many = `${one}s`) => `${n} ${n === 1 ? one : many}`;

function tallyText(t: SessionTally, names: Names): string {
  const parts: string[] = [];
  if (t.started) {
    parts.push(t.actor.kind === 'session' ? 'was started from PitCrew' : `started “${sessionName(t.root)}”`);
  }
  else if (t.found) parts.push(`was found on ${names.machine(t.root.machine)}`);
  if (t.turns > 0) parts.push(`finished ${plural(t.turns, 'turn')}`);
  if (t.tools > 0) parts.push(`ran ${plural(t.tools, 'tool')}`);
  if (t.edits > 0) parts.push(`edited ${plural(t.edits, 'file')}`);
  if (t.subagents > 0) parts.push(`started ${plural(t.subagents, 'sub-agent')}`);
  if (t.ended) parts.push('ended');
  if (parts.length === 0) parts.push('changed');
  const text = parts.join(', ');
  // A session named by its agent says which session.
  return t.actor.kind === 'member' && !t.started ? `${text} in “${sessionName(t.root)}”` : text;
}

/**
 * "Since you last looked": what others did, newest first, the person's own actions left out (a
 * session they started from PitCrew is theirs; what the session then does is not). Each session's
 * events, its sub-agents' included, fold into one line per session ("finished 2 turns, ran 3
 * tools"); everything else is a line of its own.
 */
export function changesSince(
  items: readonly { event: Event; rev: number }[],
  sessions: ReadonlyMap<SessionId, Session>,
  names: Names,
  me: MemberId | undefined,
): ChangeLine[] {
  const lines: (ChangeLine | SessionTally)[] = [];
  const tallies = new Map<SessionId, SessionTally>();
  for (const { event, rev } of byTimeNewestFirst(items)) {
    const actor = actorOf(event, sessions);
    if (actor.kind === 'member' && me !== undefined && actor.member === me) continue;
    const id = eventSession(event);
    const own = id === undefined ? undefined : (sessions.get(id) ?? (event.body.type === 'session_discovered' ? event.body.data.session : undefined));
    if (own === undefined) {
      lines.push({
        key: event.id,
        event,
        revs: [rev],
        who: actorName(actor, names.member),
        what: describeEvent(event, names),
        actor,
      });
      continue;
    }
    const root = rootOf(own, sessions);
    let tally = tallies.get(root.id);
    if (tally === undefined) {
      tally = {
        root,
        actor,
        newest: event,
        revs: [],
        found: false,
        started: false,
        turns: 0,
        tools: 0,
        edits: 0,
        subagents: 0,
        ended: false,
      };
      tallies.set(root.id, tally);
      lines.push(tally);
    }
    tally.revs.push(rev);
    const nested = aboutSubagent(event, sessions);
    switch (event.body.type) {
      case 'session_discovered':
        if (nested) tally.subagents += 1;
        else if (startedByPerson(event.body.data.session) || event.body.data.session.agent !== undefined) tally.started = true;
        else tally.found = true;
        break;
      case 'turn_ended':
        tally.turns += 1;
        break;
      case 'tool_ran':
        tally.tools += 1;
        break;
      case 'file_edited':
        tally.edits += 1;
        break;
      case 'session_ended':
        if (!nested) tally.ended = true;
        break;
      default:
        break;
    }
  }
  return lines.map((line) => {
    if (!('root' in line)) return line;
    return {
      key: `session:${line.root.id}`,
      event: line.newest,
      revs: line.revs,
      who: actorName(line.actor, names.member),
      what: tallyText(line, names),
      actor: line.actor,
    };
  });
}
