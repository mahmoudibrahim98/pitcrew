// Sub-agents and who a session's events are about (api-v1.md, "Sessions": "What a session says
// about itself" and "Who did it"). Pure and React-free: the console, Home, activity and the
// sidebar all use it, so they agree on what is an agent and what is part of one.

import type { Engine, Event, Session, SessionId } from './types.ts';

export const ENGINE_NAME: Record<Engine, string> = {
  claude: 'Claude',
  codex: 'Codex',
  opencode: 'OpenCode',
};

/** How far up a chain of parents is followed; past it (or in a loop) a session stands alone. */
const MAX_CHAIN = 16;

/** Sessions by id. */
export function byId(sessions: readonly Session[]): Map<SessionId, Session> {
  return new Map(sessions.map((s) => [s.id, s]));
}

/**
 * The top of `session`'s chain of parents among `sessions`: the first session whose parent is not
 * there (none, not found, or not imported). Undefined when the chain ends nowhere: a loop of
 * parents, or one past `MAX_CHAIN`.
 */
function topOf(session: Session, sessions: ReadonlyMap<SessionId, Session>): Session | undefined {
  let at = session;
  for (let i = 0; i < MAX_CHAIN; i += 1) {
    const parent = at.parent === undefined ? undefined : sessions.get(at.parent);
    if (parent === undefined) return at;
    at = parent;
  }
  return undefined;
}

/**
 * The session a sub-agent is nested under: its parent, when that is among `sessions` and the
 * chain of parents ends at a session of its own. A sub-agent whose parent is not there (not found,
 * or not imported), or whose chain loops, stands on its own.
 */
export function parentOf(session: Session, sessions: ReadonlyMap<SessionId, Session>): Session | undefined {
  if (session.parent === undefined) return undefined;
  const parent = sessions.get(session.parent);
  if (parent === undefined || topOf(session, sessions) === undefined) return undefined;
  return parent;
}

/** Whether `session` is nested under a parent in `sessions` (and so is no agent of its own). */
export function isNested(session: Session, sessions: ReadonlyMap<SessionId, Session>): boolean {
  return parentOf(session, sessions) !== undefined;
}

/** The sessions that are not nested under another: the agents. */
export function topLevel(sessions: readonly Session[]): Session[] {
  const all = byId(sessions);
  return sessions.filter((s) => !isNested(s, all));
}

/** The outermost session `session` is part of: the top of its chain, or itself when it stands alone. */
export function rootOf(session: Session, sessions: ReadonlyMap<SessionId, Session>): Session {
  return topOf(session, sessions) ?? session;
}

/** Each parent's sub-agents (those nested under it), oldest first. */
export function subagentsByParent(sessions: readonly Session[]): Map<SessionId, Session[]> {
  const all = byId(sessions);
  const out = new Map<SessionId, Session[]>();
  for (const s of sessions) {
    const parent = parentOf(s, all);
    if (parent === undefined) continue;
    const list = out.get(parent.id);
    if (list === undefined) out.set(parent.id, [s]);
    else list.push(s);
  }
  for (const list of out.values()) list.sort((a, b) => a.started - b.started || a.id.localeCompare(b.id));
  return out;
}

/** A session's name: its title, else its folder's. */
export function sessionName(session: Pick<Session, 'title' | 'cwd'>): string {
  if (session.title !== undefined && session.title.trim() !== '') return session.title;
  const folder = session.cwd.replace(/[\\/]+$/, '').split(/[\\/]/).at(-1);
  return folder === undefined || folder === '' ? session.cwd : folder;
}

/** The session an event is about, if it is one of the runner's session events. */
export function eventSession(event: Event): SessionId | undefined {
  const { body } = event;
  switch (body.type) {
    case 'session_discovered':
      return body.data.session.id;
    case 'session_state_changed':
    case 'turn_ended':
    case 'tool_ran':
    case 'file_edited':
    case 'session_updated':
    case 'session_ended':
      return body.data.session;
    case 'session_linked':
      // A person's link is theirs; the runner's own (by folder or branch) is the session's.
      return body.data.basis === 'folder' || body.data.basis === 'branch' ? body.data.session : undefined;
    default:
      return undefined;
  }
}

/** Who an event names as its doer. */
export type Actor =
  /** A person or agent: the event's author, or the agent running the session. */
  | { kind: 'member'; member: string }
  /** A session nobody started from PitCrew, named by its engine and itself. */
  | { kind: 'session'; session: Session; engine: Engine; name: string };

/**
 * Whether `session` was started from PitCrew by a person: it runs as no agent, and PitCrew owns its
 * terminal, or it is the hub's record of the start, made before the CLI ran (no `native_id` yet;
 * the runner states every session it finds with one). Only then is "@person started the session"
 * true.
 */
export function startedByPerson(session: Session): boolean {
  return session.agent === undefined && (session.terminal !== undefined || session.native_id === '');
}

/**
 * Activity worth a line: a session's second and later `session_discovered` (the hub's record of a
 * start, then its terminal, then the runner finding its transcript) say nothing new, so only the
 * oldest in `items` stays. `items` may be in any order; theirs is kept.
 */
export function withoutRestatements<T extends { event: Event; rev: number }>(items: readonly T[]): T[] {
  const first = new Map<SessionId, number>();
  for (const { event, rev } of items) {
    if (event.body.type !== 'session_discovered') continue;
    const id = event.body.data.session.id;
    const seen = first.get(id);
    if (seen === undefined || rev < seen) first.set(id, rev);
  }
  return items.filter(
    ({ event, rev }) => event.body.type !== 'session_discovered' || first.get(event.body.data.session.id) === rev,
  );
}

/**
 * Who did `event`. The runner's session events are stamped with the workspace's person, but they
 * are the session's doing: the agent running it when it has one (a dispatch's), otherwise the
 * session itself (engine and name). A sub-agent's are its parent's. A person's own start of a
 * session is theirs. Everything else is its author's.
 */
export function actorOf(event: Event, sessions: ReadonlyMap<SessionId, Session>): Actor {
  const id = eventSession(event);
  if (id === undefined) return { kind: 'member', member: event.author };
  const stated = event.body.type === 'session_discovered' ? event.body.data.session : undefined;
  const own = sessions.get(id) ?? stated;
  if (own === undefined) return { kind: 'member', member: event.author };
  const root = rootOf(own, sessions);
  if (root.agent !== undefined) return { kind: 'member', member: root.agent };
  if (root.id === own.id && stated !== undefined && (startedByPerson(stated) || startedByPerson(own))) {
    return { kind: 'member', member: event.author };
  }
  return { kind: 'session', session: root, engine: root.engine, name: sessionName(root) };
}

/** An actor's display name: a member's from `member`, a session's as "Claude · its name". */
export function actorName(actor: Actor, member: (id: string) => string): string {
  return actor.kind === 'member' ? member(actor.member) : `${ENGINE_NAME[actor.engine]} · ${actor.name}`;
}

/** Whether `event` is about a sub-agent folded into its parent's activity. */
export function aboutSubagent(event: Event, sessions: ReadonlyMap<SessionId, Session>): boolean {
  const id = eventSession(event);
  const session = id === undefined ? undefined : sessions.get(id);
  return session !== undefined && isNested(session, sessions);
}

/**
 * Events newest first, by when they happened; the same moment keeps the log's order. A time ahead
 * of `now` (a machine whose clock runs fast) counts as now, so it cannot hold the top of the list.
 */
export function byTimeNewestFirst<T extends { event: Event; rev?: number }>(
  items: readonly T[],
  now: number = Date.now(),
): T[] {
  const when = (event: Event) => Math.min(event.at, now);
  return items
    .map((item, i) => ({ item, i }))
    .sort((a, b) => when(b.item.event) - when(a.item.event) || (b.item.rev ?? b.i) - (a.item.rev ?? a.i))
    .map(({ item }) => item);
}
