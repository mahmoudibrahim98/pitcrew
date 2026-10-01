// Optimistic moves. A moved card shows in its new column at once; the hub's answer decides what
// happens next:
// - refused: the card goes back, with the hub's message (one notice per task);
// - accepted: the card stays where it was put until the task list shows the task somewhere other
//   than where it started (the `task_moved` event's refresh, or someone else's later move). A slow
//   refresh that still shows the old column does not move it back. If the list never catches up
//   within `MOVE_FALLBACK_MS` (the stream's reconnect window), the card shows what the list says.
// The cache is never written here: the events do that.

import { hashKey, useQueryClient, type QueryKey } from '@tanstack/react-query';
import { useEffect, useRef, useState } from 'react';
import type { Task, TaskId, TaskStatus } from '../data/index.ts';
import { TASK_STATUS } from './format.ts';

/** How long an accepted move outranks a task list that still shows the old status. */
export const MOVE_FALLBACK_MS = 60_000;

export interface PendingMove {
  /** Tells this move from a later one of the same task. */
  id: number;
  from: TaskStatus;
  to: TaskStatus;
  /** The hub accepted it. */
  accepted: boolean;
}

export type PendingMoves = ReadonlyMap<TaskId, PendingMove>;

export interface MoveNotice {
  key: string;
  text: string;
}

/** Where a card shows: the move's target while it is in flight or not yet in the list. */
export function shownStatus(task: Task, move: PendingMove | undefined): TaskStatus {
  if (move === undefined) return task.status;
  if (!move.accepted) return move.to;
  return task.status === move.from ? move.to : task.status;
}

/** Drops accepted moves the list has caught up with (or moved past). Same map if none. */
export function settle(moves: PendingMoves, tasks: readonly Task[]): PendingMoves {
  let next: Map<TaskId, PendingMove> | undefined;
  for (const task of tasks) {
    const move = moves.get(task.id);
    if (move?.accepted === true && task.status !== move.from) {
      next ??= new Map(moves);
      next.delete(task.id);
    }
  }
  return next ?? moves;
}

function without<V>(map: ReadonlyMap<TaskId, V>, task: TaskId): ReadonlyMap<TaskId, V> {
  if (!map.has(task)) return map;
  const next = new Map(map);
  next.delete(task);
  return next;
}

export function useOptimisticMoves({
  send,
  listKey,
  fallbackMs = MOVE_FALLBACK_MS,
}: {
  /** Asks the hub; resolves when it accepts, rejects with its reason. */
  send: (task: Task, to: TaskStatus) => Promise<unknown>;
  /** The task list the cards come from. */
  listKey: QueryKey;
  fallbackMs?: number;
}) {
  const queryClient = useQueryClient();
  const [moves, setMoves] = useState<PendingMoves>(new Map());
  const [notices, setNotices] = useState<ReadonlyMap<TaskId, MoveNotice>>(new Map());
  const lastId = useRef(0);
  const timers = useRef(new Set<ReturnType<typeof setTimeout>>());
  const listHash = hashKey(listKey);

  // Settle accepted moves as the list changes, whichever way it changed (refetch or patch).
  useEffect(
    () =>
      queryClient.getQueryCache().subscribe((event) => {
        if (event.type !== 'updated' || event.query.queryHash !== listHash) return;
        const data: unknown = event.query.state.data;
        if (Array.isArray(data)) setMoves((m) => settle(m, data as Task[]));
      }),
    [queryClient, listHash],
  );

  useEffect(() => {
    const pending = timers.current;
    return () => {
      for (const timer of pending) clearTimeout(timer);
      pending.clear();
    };
  }, []);

  const forget = (task: TaskId, id: number) =>
    setMoves((m) => (m.get(task)?.id === id ? without(m, task) : m));

  const move = (task: Task, to: TaskStatus) => {
    if (shownStatus(task, moves.get(task.id)) === to) return;
    lastId.current += 1;
    const id = lastId.current;
    const from = task.status;
    setMoves((m) => new Map(m).set(task.id, { id, from, to, accepted: false }));
    setNotices((n) => without(n, task.id));
    send(task, to).then(
      () => {
        // The event may have beaten the answer: then there is nothing left to hold.
        const listed = queryClient.getQueryData<Task[]>(listKey)?.find((t) => t.id === task.id);
        if (listed !== undefined && listed.status !== from) {
          forget(task.id, id);
          return;
        }
        setMoves((m) => (m.get(task.id)?.id === id ? new Map(m).set(task.id, { id, from, to, accepted: true }) : m));
        const timer = setTimeout(() => {
          timers.current.delete(timer);
          forget(task.id, id);
        }, fallbackMs);
        timers.current.add(timer);
      },
      (error: unknown) => {
        forget(task.id, id);
        const reason = error instanceof Error ? error.message : String(error);
        setNotices((n) =>
          new Map(n).set(task.id, {
            key: task.key,
            text: `Couldn’t move ${task.key} to ${TASK_STATUS[to].label}: ${reason}`,
          }),
        );
      },
    );
  };

  return {
    moves,
    notices,
    statusOf: (task: Task) => shownStatus(task, moves.get(task.id)),
    /** Tasks whose move the hub has not answered yet. */
    inFlight: new Set([...moves].filter(([, m]) => !m.accepted).map(([task]) => task)),
    move,
    dismiss: (task: TaskId) => setNotices((n) => without(n, task)),
  };
}
