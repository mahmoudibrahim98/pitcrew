// Product rules ported from `crates/protocol/src/model.rs`.

import type { Mover, TaskStatus } from './types.ts';

/**
 * Whether `mover` may move a task from `from` to `to` (`TaskStatus::can_move`, ADR-0007):
 * - people may make any move;
 * - agents may move only their own task, and only forward: backlog or todo → in progress, and
 *   in progress → review;
 * - the back office moves in progress → review, and review → done only with `accept_auto`;
 * - sync mirrors an upstream close (to done) or reopen (done → todo), never touching a task that
 *   is in progress.
 *
 * Moving a task to the status it already has is never allowed.
 */
export function canMove(from: TaskStatus, to: TaskStatus, mover: Mover): boolean {
  if (from === to) {
    return false;
  }
  switch (mover.kind) {
    case 'person':
      return true;
    case 'agent':
      return (
        mover.on_own_task &&
        (((from === 'backlog' || from === 'todo') && to === 'in_progress') ||
          (from === 'in_progress' && to === 'review'))
      );
    case 'back_office':
      return (
        (from === 'in_progress' && to === 'review') ||
        (from === 'review' && to === 'done' && mover.accept_auto)
      );
    case 'sync':
      return (
        (to === 'done' &&
          (from === 'backlog' || from === 'todo' || from === 'review' || from === 'canceled')) ||
        (from === 'done' && to === 'todo')
      );
  }
}

/** `Date::is_well_formed`: `YYYY-MM-DD` with a month of 1–12 and a day of 1–31. */
export function isWellFormedDate(text: string): boolean {
  const match = /^(\d{4})-(\d{2})-(\d{2})$/.exec(text);
  if (match === null) {
    return false;
  }
  const month = Number(match[2]);
  const day = Number(match[3]);
  return month >= 1 && month <= 12 && day >= 1 && day <= 31;
}
