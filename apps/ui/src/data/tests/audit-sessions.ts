// Sessions shaped like the audit of 5 October 2026's synthetic homes, for tests: ten sessions
// in three repositories (one with two worktrees), and four sub-agents nested under three of them.
// Every id, path and name is made up.

import type { Session } from '../types.ts';

const MACHINE = '01JB000000000000000MCH0001';

let next = 0;

/** A synthetic session, idle, in the `atlas` repository unless `fields` say otherwise. */
export function session(native: string, fields: Partial<Session> = {}): Session {
  next += 1;
  return {
    id: `01JB00000000000000SES${String(next).padStart(5, '0')}`,
    engine: 'claude',
    native_id: native,
    machine: MACHINE,
    cwd: '/home/sam/work/atlas',
    title: native,
    state: 'idle',
    started: 1_790_000_000_000 + next * 60_000,
    last_activity: 1_790_000_000_000 + next * 60_000,
    ...fields,
  };
}

/** Ten sessions, then four sub-agents: two of `c-atlas`, one of `c-search`, one of `x-atlas`. */
export function auditSessions(): { sessions: Session[]; parents: Record<'atlas' | 'search' | 'codex', Session> } {
  const atlas = session('c-atlas');
  const search = session('c-search', { cwd: '/home/sam/work/atlas/.claude/worktrees/search' });
  const codex = session('x-atlas', { engine: 'codex' });
  const top = [
    atlas,
    session('c-atlas-src', { cwd: '/home/sam/work/atlas/src' }),
    search,
    session('c-hotfix', { cwd: '/home/sam/work/atlas-hotfix' }),
    session('c-beacon', { cwd: '/home/sam/work/beacon' }),
    session('c-beacon-docs', { cwd: '/home/sam/work/beacon/docs' }),
    session('c-compass', { cwd: '/home/sam/work/compass' }),
    codex,
    session('x-exec', { engine: 'codex', cwd: '/home/sam/work/compass' }),
    session('x-beacon', { engine: 'codex', cwd: '/home/sam/work/beacon' }),
  ];
  const subs = [
    session('a1', { parent: atlas.id, state: 'working' }),
    session('a2', { parent: atlas.id, state: 'working' }),
    session('s1', { parent: search.id, cwd: search.cwd, state: 'working' }),
    session('x-sub', { engine: 'codex', parent: codex.id, state: 'working' }),
  ];
  return { sessions: [...top, ...subs], parents: { atlas, search, codex } };
}
