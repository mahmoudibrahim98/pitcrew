import { QueryClient } from '@tanstack/react-query';
import { expect, it } from 'vitest';
import { keys } from '../keys.ts';
import { applyPatches } from '../patches.ts';
import type { Session } from '../types.ts';

it('keeps failed dispatches ended on rediscovery in the detail, list and active count', () => {
  const qc = new QueryClient();
  const session: Session = { id: '01JB000000000000000SES0001', engine: 'codex', native_id: '',
    machine: '01JB000000000000000MCH0001', cwd: '/home/sam/work', state: 'starting', started: 1, last_activity: 1,
    workstream: '01JB000000000000000WST0001', task: '01JB000000000000000TSK0001', link_basis: 'dispatch',
    agent: '01JB000000000000000MEM0001' };
  qc.setQueryData(keys.sessions.detail(session.id), session);
  qc.setQueryData(keys.sessions.list(), [session]);
  qc.setQueryData(keys.sessions.list({ state: 'starting' }), [session]);
  applyPatches(qc, [{ body: { type: 'session_ended', data: { session: session.id } } }]);
  const stale: Session = { id: session.id, engine: 'codex', native_id: 'native', machine: session.machine,
    cwd: session.cwd, state: 'starting', started: 1, last_activity: 1, status_line: 'starting' };
  applyPatches(qc, [{ body: { type: 'session_discovered', data: { session: stale } } }]);
  const expected = { ...session, state: 'ended', native_id: 'native' };
  expect(qc.getQueryData(keys.sessions.detail(session.id))).toEqual(expected);
  expect(qc.getQueryData(keys.sessions.list())).toEqual([expected]);
  expect(qc.getQueryData(keys.sessions.list({ state: 'starting' }))).toEqual([]);
  applyPatches(qc, [{ body: { type: 'session_state_changed', data: { session: session.id, from: 'ended', to: 'working' } } }]);
  expect(qc.getQueryData<Session>(keys.sessions.detail(session.id))?.state).toBe('working');
  qc.clear();
});
