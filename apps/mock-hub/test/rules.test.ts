import assert from 'node:assert/strict';
import { describe, it } from 'node:test';
import { canMove, isWellFormedDate } from '../src/rules.ts';
import { TASK_STATUSES, type Mover, type TaskStatus } from '../src/types.ts';
import { isUlid, ulid } from '../src/ulid.ts';

/** Every (from, to) pair a mover may make, as `from→to`. */
function allowed(mover: Mover): string[] {
  const moves: string[] = [];
  for (const from of TASK_STATUSES) {
    for (const to of TASK_STATUSES) {
      if (canMove(from, to, mover)) {
        moves.push(`${from}→${to}`);
      }
    }
  }
  return moves;
}

describe('TaskStatus::can_move, ported', () => {
  it('lets people make any move except to the same status', () => {
    assert.equal(allowed({ kind: 'person' }).length, 6 * 5);
    for (const status of TASK_STATUSES) {
      assert.equal(canMove(status, status, { kind: 'person' }), false);
    }
  });

  it('lets agents move only their own task, and only forward', () => {
    assert.deepEqual(allowed({ kind: 'agent', on_own_task: true }), [
      'backlog→in_progress',
      'todo→in_progress',
      'in_progress→review',
    ]);
    assert.deepEqual(allowed({ kind: 'agent', on_own_task: false }), []);
  });

  it('lets the back office move to review, and accept only when allowed', () => {
    assert.deepEqual(allowed({ kind: 'back_office', accept_auto: false }), ['in_progress→review']);
    assert.deepEqual(allowed({ kind: 'back_office', accept_auto: true }), [
      'in_progress→review',
      'review→done',
    ]);
  });

  it('lets sync close and reopen, never touching work in progress', () => {
    const moves = allowed({ kind: 'sync' });
    assert.deepEqual(moves, [
      'backlog→done',
      'todo→done',
      'review→done',
      'done→todo',
      'canceled→done',
    ]);
    const inProgress: TaskStatus = 'in_progress';
    assert.equal(moves.some((m) => m.startsWith(inProgress)), false);
  });
});

describe('dates and ids', () => {
  it('checks dates the way Date::is_well_formed does', () => {
    for (const good of ['2026-10-10', '0001-01-31', '2026-12-01']) {
      assert.equal(isWellFormedDate(good), true, good);
    }
    for (const bad of ['2026-13-01', '2026-00-10', '2026-10-32', '2026-1-01', '26-10-10', '2026/10/10']) {
      assert.equal(isWellFormedDate(bad), false, bad);
    }
  });

  it('makes time-ordered ULIDs in Crockford base32', () => {
    const start = Date.now();
    const ids = Array.from({ length: 2000 }, () => ulid());
    for (const id of ids) {
      assert.equal(isUlid(id), true, id);
    }
    assert.deepEqual([...ids].sort(), ids, 'ids sort in creation order, even within one millisecond');
    assert.equal(new Set(ids).size, ids.length);
    const alphabet = '0123456789ABCDEFGHJKMNPQRSTVWXYZ';
    const time = [...(ids[0] ?? '').slice(0, 10)].reduce((t, c) => t * 32 + alphabet.indexOf(c), 0);
    assert.ok(time >= start && time <= Date.now(), 'the first 10 characters are the time in ms');
    assert.ok(ulid() > '01JB000000000000000EVT0015', 'new ids sort after the fixture ids');
  });
});
