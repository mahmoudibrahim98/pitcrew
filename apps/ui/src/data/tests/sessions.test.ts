// Sub-agents and attribution (api-v1.md, "Sessions"): on sessions shaped like the audit's
// synthetic homes, ten sessions and four sub-agents, no sub-agent is an agent, and the runner's
// events name the session or its agent, never the person who did not start it.

import { describe, expect, it } from 'vitest';
import {
  actorName,
  actorOf,
  byId,
  byTimeNewestFirst,
  isNested,
  rootOf,
  subagentsByParent,
  topLevel,
} from '../sessions.ts';
import type { Event, EventBody } from '../types.ts';
import { auditSessions, session } from './audit-sessions.ts';

const ALEX = '01JB000000000000000MEM0001';
const WRITER = '01JB000000000000000MEM0002';
let next = 0;

function event(body: EventBody, at: number, author = ALEX): Event {
  next += 1;
  return { id: `01JB0000000000000000EV${String(next).padStart(4, '0')}`, at, workspace: 'w', author, body };
}

describe('sub-agents', () => {
  it('are nested under their parents and are no agents of their own', () => {
    const { sessions, parents } = auditSessions();
    const agents = topLevel(sessions);
    expect(agents).toHaveLength(10);
    expect(agents.some((s) => s.parent !== undefined)).toBe(false);
    const children = subagentsByParent(sessions);
    expect(children.get(parents.atlas.id)?.map((s) => s.native_id)).toEqual(['a1', 'a2']);
    expect(children.get(parents.search.id)?.map((s) => s.native_id)).toEqual(['s1']);
    expect(children.get(parents.codex.id)?.map((s) => s.native_id)).toEqual(['x-sub']);
    const all = byId(sessions);
    const a1 = sessions.find((s) => s.native_id === 'a1');
    expect(a1 && rootOf(a1, all).id).toBe(parents.atlas.id);
  });

  it('stand on their own when their parent is not there', () => {
    const orphan = session('orphan', { parent: '01JB00000000000000SES99999' });
    expect(isNested(orphan, byId([orphan]))).toBe(false);
    expect(topLevel([orphan])).toEqual([orphan]);
    const loop = session('loop');
    loop.parent = loop.id;
    expect(topLevel([loop])).toEqual([loop]);
  });

  it('nest only when their chain of parents ends: a loop hides no one', () => {
    const a = session('a');
    const b = session('b', { parent: a.id });
    a.parent = b.id;
    const child = session('child', { parent: a.id });
    const all = [a, b, child];
    // Both of the loop, and what hangs off it, stand on their own: none is hidden.
    expect(topLevel(all).map((s) => s.native_id)).toEqual(['a', 'b', 'child']);
    expect(subagentsByParent(all).size).toBe(0);
    expect(rootOf(child, byId(all)).id).toBe(child.id);
    // A chain that ends at a session whose parent is not there nests under that session.
    const top = session('top', { parent: '01JB00000000000000SES99998' });
    const mid = session('mid', { parent: top.id });
    const leaf = session('leaf', { parent: mid.id });
    expect(topLevel([top, mid, leaf]).map((s) => s.native_id)).toEqual(['top']);
    expect(rootOf(leaf, byId([top, mid, leaf])).id).toBe(top.id);
  });
});

describe('who did it', () => {
  it('names the session, not the person, for a session found on disk', () => {
    const found = session('found');
    const all = byId([found]);
    const discovered = event({ type: 'session_discovered', data: { session: found } }, 1);
    const turn = event({ type: 'turn_ended', data: { session: found.id, receipt: { kind: 'transcript', session: found.id, offset: 0 } } }, 2);
    for (const e of [discovered, turn]) {
      const actor = actorOf(e, all);
      expect(actor.kind).toBe('session');
      expect(actorName(actor, () => '@alex')).toBe('Claude · found');
    }
  });

  it('names the person only when they started it from PitCrew, and the agent of a dispatch', () => {
    const started = session('started', { terminal: '01JB000000000000000TRM0001' });
    const dispatched = session('dispatched', { agent: WRITER, terminal: '01JB000000000000000TRM0002' });
    const all = byId([started, dispatched]);
    expect(actorOf(event({ type: 'session_discovered', data: { session: started } }, 1), all)).toEqual({
      kind: 'member',
      member: ALEX,
    });
    // What the session then does is its own.
    const ran = event({ type: 'tool_ran', data: { session: started.id, tool: 'Bash', target: 'ls', outcome: 'ok', failed: false, receipt: { kind: 'transcript', session: started.id, offset: 1 } } }, 2);
    expect(actorOf(ran, all).kind).toBe('session');
    for (const body of [
      { type: 'session_discovered', data: { session: dispatched } },
      { type: 'session_state_changed', data: { session: dispatched.id, from: 'starting', to: 'working' } },
    ] satisfies EventBody[]) {
      expect(actorOf(event(body, 3), all)).toEqual({ kind: 'member', member: WRITER });
    }
  });

  it('folds a sub-agent into its parent', () => {
    const { sessions, parents } = auditSessions();
    const all = byId(sessions);
    const a1 = sessions.find((s) => s.native_id === 'a1');
    if (a1 === undefined) throw new Error('no a1');
    const actor = actorOf(event({ type: 'turn_ended', data: { session: a1.id, receipt: { kind: 'transcript', session: a1.id, offset: 0 } } }, 1), all);
    expect(actor.kind === 'session' && actor.session.id).toBe(parents.atlas.id);
  });

  it('leaves everything else to its author', () => {
    const e = event({ type: 'project_created', data: { project: { id: 'p', key: 'AT', name: 'atlas', status: 'in_progress', lead: ALEX, members: [ALEX], external: [] } } }, 1);
    expect(actorOf(e, new Map())).toEqual({ kind: 'member', member: ALEX });
  });
});

describe('order', () => {
  it('is by when things happened, not by the log', () => {
    const imported = [
      { event: event({ type: 'session_ended', data: { session: 'a' } }, 300), rev: 1 },
      { event: event({ type: 'session_ended', data: { session: 'b' } }, 100), rev: 2 },
      { event: event({ type: 'session_ended', data: { session: 'c' } }, 200), rev: 3 },
      { event: event({ type: 'session_ended', data: { session: 'd' } }, 200), rev: 4 },
    ];
    expect(byTimeNewestFirst(imported).map((i) => i.rev)).toEqual([1, 4, 3, 2]);
  });

  it('counts a time ahead of now as now, so a fast clock does not hold the top', () => {
    const now = 1_000;
    const items = [
      { event: event({ type: 'session_ended', data: { session: 'fast' } }, 1_000_000), rev: 1 },
      { event: event({ type: 'session_ended', data: { session: 'later' } }, 900), rev: 2 },
      { event: event({ type: 'session_ended', data: { session: 'newest' } }, 1_000), rev: 3 },
    ];
    // The fast one reads as now: with what happened now, by the log; above what came before.
    expect(byTimeNewestFirst(items, now).map((i) => i.rev)).toEqual([3, 1, 2]);
  });
});
