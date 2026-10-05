// Activity and "Since you last looked" on the audit's shape: ordered by when things happened, a
// found session named by itself (never "@alex started the session"), a dispatch by its agent,
// sub-agents folded into their parents, and the person's own actions left out.

import { describe, expect, it } from 'vitest';
import { sessionsById, type Event, type EventBody } from '../../data/index.ts';
import { auditSessions, session } from '../../data/tests/audit-sessions.ts';
import { attributedFeed, changesSince } from '../attribution.ts';
import { plainNames, type Names } from '../format.ts';

const ALEX = '01JB000000000000000MEM0001';
const WRITER = '01JB000000000000000MEM0002';

const names: Names = {
  ...plainNames,
  member: (id) => (id === ALEX ? '@alex' : id === WRITER ? '@writer' : id),
  machine: () => 'This laptop',
};

let next = 0;
function event(body: EventBody, at: number, author = ALEX): Event {
  next += 1;
  return { id: `01JB0000000000000000EV${String(next).padStart(4, '0')}`, at, workspace: 'w', author, body };
}
const receipt = (s: string) => ({ kind: 'transcript' as const, session: s, offset: 0 });

describe('the activity feed', () => {
  it('orders by time and names who really did it', () => {
    const { sessions, parents } = auditSessions();
    const a1 = sessions.find((s) => s.native_id === 'a1');
    if (a1 === undefined) throw new Error('no a1');
    const dispatched = session('dispatched', { agent: WRITER, terminal: '01JB000000000000000TRM0001' });
    const all = sessionsById([...sessions, dispatched]);
    // Imported history: the log's order is not the order things happened in.
    const items = [
      { event: event({ type: 'session_discovered', data: { session: parents.atlas } }, 1_000), rev: 1 },
      { event: event({ type: 'turn_ended', data: { session: parents.atlas.id, receipt: receipt(parents.atlas.id) } }, 5_000), rev: 2 },
      { event: event({ type: 'session_discovered', data: { session: a1 } }, 2_000), rev: 3 },
      { event: event({ type: 'tool_ran', data: { session: a1.id, tool: 'Bash', target: 'ls', outcome: 'ok', failed: false, receipt: receipt(a1.id) } }, 3_000), rev: 4 },
      { event: event({ type: 'session_discovered', data: { session: dispatched } }, 4_000), rev: 5 },
    ];
    const feed = attributedFeed(items, all, names);
    expect(feed.map((f) => f.rev)).toEqual([2, 5, 4, 3, 1]);
    expect(feed.map((f) => `${f.who} ${f.what}`)).toEqual([
      'Claude · c-atlas finished a turn',
      '@writer started the session “dispatched”',
      'Claude · c-atlas ran Bash ls: ok (in a sub-agent “a1”)',
      'Claude · c-atlas started a sub-agent “a1”',
      'Claude · c-atlas was found on This laptop',
    ]);
    expect(feed.some((f) => f.who === '@alex')).toBe(false);
  });
});

describe('since you last looked', () => {
  it('leaves out my own actions and folds each session into one line', () => {
    const { sessions, parents } = auditSessions();
    const a1 = sessions.find((s) => s.native_id === 'a1');
    if (a1 === undefined) throw new Error('no a1');
    const mine = session('mine', { terminal: '01JB000000000000000TRM0002' });
    const all = sessionsById([...sessions, mine]);
    const items = [
      // My own: a project I made and a session I started from PitCrew.
      { event: event({ type: 'project_created', data: { project: { id: 'p', key: 'AT', name: 'atlas', status: 'in_progress', lead: ALEX, members: [ALEX], external: [] } } }, 100), rev: 1 },
      { event: event({ type: 'session_discovered', data: { session: mine } }, 110), rev: 2 },
      // The session I started then works on its own.
      { event: event({ type: 'turn_ended', data: { session: mine.id, receipt: receipt(mine.id) } }, 120), rev: 3 },
      // A found session's turns and its sub-agent's: one line.
      { event: event({ type: 'turn_ended', data: { session: parents.atlas.id, receipt: receipt(parents.atlas.id) } }, 200), rev: 4 },
      { event: event({ type: 'turn_ended', data: { session: parents.atlas.id, receipt: receipt(parents.atlas.id) } }, 300), rev: 5 },
      { event: event({ type: 'session_discovered', data: { session: a1 } }, 210), rev: 6 },
      { event: event({ type: 'tool_ran', data: { session: a1.id, tool: 'Bash', target: 'ls', outcome: 'ok', failed: false, receipt: receipt(a1.id) } }, 220), rev: 7 },
      // Someone else's comment.
      { event: event({ type: 'comment_posted', data: { text: 'Looks good', mentions: [] } }, 250, WRITER), rev: 8 },
    ];
    const lines = changesSince(items, all, names, ALEX);
    expect(lines.map((l) => `${l.who} ${l.what}`)).toEqual([
      'Claude · c-atlas finished 2 turns, ran 1 tool, started 1 sub-agent',
      '@writer commented: Looks good',
      'Claude · mine finished 1 turn',
    ]);
    expect(lines[0]?.revs.sort((a, b) => a - b)).toEqual([4, 5, 6, 7]);
  });
});
