// @vitest-environment happy-dom
// @vitest-environment-options {"url": "http://localhost:5173/"}
//
// "Agents now" on the audit's shape (ten sessions, four sub-agents at work): no sub-agent is an
// agent; active sessions first (a quiet found session is not active), then at most a few recent
// ones, with "Show all".

import { fireEvent, screen, within } from '@testing-library/react';
import { afterEach, beforeEach, expect, it } from 'vitest';
import type { Session } from '../../data/index.ts';
import { auditSessions } from '../../data/tests/audit-sessions.ts';
import { AgentsNow, RECENT_SHOWN, isActive } from '../agents.tsx';
import { renderWithHub, startHub, stopHub, type Hub } from './harness.tsx';

let hub: Hub;
beforeEach(async () => {
  hub = await startHub();
});
afterEach(async () => {
  await stopHub(hub);
});

/** Answers `GET /v1/sessions` with `sessions` (as the hub filters them); passes everything else on to the hub. */
function serving(sessions: Session[]): typeof fetch {
  return (async (input, init) => {
    const href = typeof input === 'string' ? input : input instanceof Request ? input.url : input.toString();
    const url = new URL(href);
    if (url.pathname === '/v1/sessions') {
      const workstream = url.searchParams.get('workstream');
      const listed = workstream === null ? sessions : sessions.filter((s) => s.workstream === workstream);
      return new Response(JSON.stringify(listed), { status: 200, headers: { 'Content-Type': 'application/json' } });
    }
    return fetch(input, init);
  }) as typeof fetch;
}

it('tells sessions at work from quiet ones', () => {
  const s = auditSessions().sessions[0];
  if (s === undefined) throw new Error('no session');
  expect(isActive({ ...s, state: 'working' })).toBe(true);
  expect(isActive({ ...s, state: 'waiting' })).toBe(true);
  expect(isActive({ ...s, state: 'starting' })).toBe(true);
  // A found session goes idle when quiet and never ends: not at work.
  expect(isActive({ ...s, state: 'idle' })).toBe(false);
  // Idle in a terminal PitCrew keeps open: its person may type next.
  expect(isActive({ ...s, state: 'idle', terminal: '01JB000000000000000TRM0001' })).toBe(true);
  expect(isActive({ ...s, state: 'unreachable' })).toBe(false);
  expect(isActive({ ...s, state: 'ended' })).toBe(false);
});

it('lists no sub-agent as an agent, active sessions first, a few recent ones, then all', async () => {
  const { sessions } = auditSessions();
  // Four of the ten are at work; the rest are quiet (found sessions go idle and stay so) or ended.
  // The sub-agents are all working.
  const shaped = sessions.map((s, i) =>
    s.parent !== undefined
      ? s
      : { ...s, state: i < 4 ? ('working' as const) : i % 2 === 0 ? ('idle' as const) : ('ended' as const) },
  );
  renderWithHub(<AgentsNow />, hub, { fetch: serving(shaped) });
  const list = await screen.findByRole('list', { name: 'Agents now' });
  const listed = () => within(list).getAllByRole('listitem').map((li) => li.getAttribute('data-session'));
  const subagents = new Set(shaped.filter((s) => s.parent !== undefined).map((s) => s.id));
  const active = shaped.filter((s) => s.parent === undefined && s.state === 'working').map((s) => s.id);
  expect(active.length).toBeGreaterThan(0);
  expect(shaped.some((s) => s.parent === undefined && s.state === 'idle')).toBe(true);
  expect(listed()).toHaveLength(active.length + RECENT_SHOWN);
  expect(listed().slice(0, active.length).sort()).toEqual([...active].sort());
  expect(listed().some((id) => id !== null && subagents.has(id))).toBe(false);

  fireEvent.click(screen.getByRole('button', { name: 'Show all (10)' }));
  expect(listed()).toHaveLength(10);
  expect(listed().some((id) => id !== null && subagents.has(id))).toBe(false);
  fireEvent.click(screen.getByRole('button', { name: 'Show fewer' }));
  expect(listed()).toHaveLength(active.length + RECENT_SHOWN);
});

it('lists no sub-agent in a worktree’s agents whose parent works in the main checkout', async () => {
  const { sessions, parents } = auditSessions();
  const main = '01JB000000000000000WST0001';
  const worktree = '01JB000000000000000WST0002';
  // The parent is linked to the main checkout's workstream; one of its sub-agents ran in the
  // worktree's folder, and is linked there.
  const sub = sessions.find((s) => s.parent === parents.atlas.id);
  if (sub === undefined) throw new Error('no sub-agent');
  const linked = sessions.map((s) =>
    s.id === sub.id
      ? { ...s, workstream: worktree, state: 'working' as const }
      : s.id === parents.search.id
        ? { ...s, workstream: worktree, state: 'working' as const }
        : { ...s, workstream: main },
  );
  renderWithHub(<AgentsNow workstream={worktree} title="Agents" />, hub, { fetch: serving(linked) });
  const list = await screen.findByRole('list', { name: 'Agents' });
  const listed = within(list)
    .getAllByRole('listitem')
    .map((li) => li.getAttribute('data-session'));
  expect(listed).toEqual([parents.search.id]);
});
