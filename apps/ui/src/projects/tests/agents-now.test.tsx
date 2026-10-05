// @vitest-environment happy-dom
// @vitest-environment-options {"url": "http://localhost:5173/"}
//
// "Agents now" on the audit's shape (ten sessions, four sub-agents at work): no sub-agent is an
// agent; active sessions first, then at most a few recent ones, with "Show all".

import { fireEvent, screen, within } from '@testing-library/react';
import { afterEach, beforeEach, expect, it } from 'vitest';
import type { Session } from '../../data/index.ts';
import { auditSessions } from '../../data/tests/audit-sessions.ts';
import { AgentsNow, RECENT_SHOWN } from '../agents.tsx';
import { renderWithHub, startHub, stopHub, type Hub } from './harness.tsx';

let hub: Hub;
beforeEach(async () => {
  hub = await startHub();
});
afterEach(async () => {
  await stopHub(hub);
});

/** Answers `GET /v1/sessions` with `sessions`; passes everything else on to the hub. */
function serving(sessions: Session[]): typeof fetch {
  return (async (input, init) => {
    const href = typeof input === 'string' ? input : input instanceof Request ? input.url : input.toString();
    if (new URL(href).pathname === '/v1/sessions') {
      return new Response(JSON.stringify(sessions), { status: 200, headers: { 'Content-Type': 'application/json' } });
    }
    return fetch(input, init);
  }) as typeof fetch;
}

it('lists no sub-agent as an agent, active sessions first, a few recent ones, then all', async () => {
  const { sessions } = auditSessions();
  // Four of the ten are at work; the rest ended, oldest first. The sub-agents are all working.
  const shaped = sessions.map((s, i) =>
    s.parent !== undefined ? s : { ...s, state: i < 4 ? ('working' as const) : ('ended' as const) },
  );
  renderWithHub(<AgentsNow />, hub, { fetch: serving(shaped) });
  const list = await screen.findByRole('list', { name: 'Agents now' });
  const listed = () => within(list).getAllByRole('listitem').map((li) => li.getAttribute('data-session'));
  const subagents = new Set(shaped.filter((s) => s.parent !== undefined).map((s) => s.id));
  const active = shaped.filter((s) => s.parent === undefined && s.state !== 'ended').map((s) => s.id);
  expect(listed()).toHaveLength(active.length + RECENT_SHOWN);
  expect(listed().slice(0, active.length).sort()).toEqual([...active].sort());
  expect(listed().some((id) => id !== null && subagents.has(id))).toBe(false);

  fireEvent.click(screen.getByRole('button', { name: 'Show all (10)' }));
  expect(listed()).toHaveLength(10);
  expect(listed().some((id) => id !== null && subagents.has(id))).toBe(false);
  fireEvent.click(screen.getByRole('button', { name: 'Show fewer' }));
  expect(listed()).toHaveLength(active.length + RECENT_SHOWN);
});
