// @vitest-environment happy-dom

import { QueryClient, QueryClientProvider } from '@tanstack/react-query';
import { act, cleanup, renderHook } from '@testing-library/react';
import type { ReactNode } from 'react';
import { afterEach, describe, expect, it, vi } from 'vitest';
import type { Task, TaskStatus } from '../../data/index.ts';
import { settle, shownStatus, useOptimisticMoves } from '../moves.ts';

const KEY = ['tasks', 'list', { project: 'P' }] as const;

const task = (id: string, status: TaskStatus): Task => ({
  id,
  key: `P-${id}`,
  project: 'P',
  title: id,
  description: '',
  status,
  priority: 'none',
  labels: [],
  blocked_by: [],
  accept_auto: false,
  subtasks: [],
});

interface Answer {
  resolve: (value: unknown) => void;
  reject: (reason: unknown) => void;
}

function setup(fallbackMs?: number) {
  const queryClient = new QueryClient();
  queryClient.setQueryData(KEY, [task('A', 'todo'), task('B', 'review')]);
  const answers: Answer[] = [];
  const send = () => new Promise((resolve, reject) => answers.push({ resolve, reject }));
  const wrapper = ({ children }: { children: ReactNode }) => (
    <QueryClientProvider client={queryClient}>{children}</QueryClientProvider>
  );
  const hook = renderHook(
    () => useOptimisticMoves({ send, listKey: KEY, ...(fallbackMs === undefined ? {} : { fallbackMs }) }),
    { wrapper },
  );
  const listed = (id: string) => {
    const found = queryClient.getQueryData<Task[]>(KEY)?.find((t) => t.id === id);
    if (found === undefined) throw new Error(`no ${id}`);
    return found;
  };
  return {
    hook,
    answers,
    shown: (id: string) => hook.result.current.statusOf(listed(id)),
    move: (id: string, to: TaskStatus) => act(() => hook.result.current.move(listed(id), to)),
    /** The list as a refresh or a patch leaves it. */
    list: (id: string, status: TaskStatus) =>
      act(() => {
        queryClient.setQueryData<Task[]>(KEY, (tasks) => tasks?.map((t) => (t.id === id ? { ...t, status } : t)));
      }),
    answer: async (index: number, error?: Error) => {
      await act(async () => {
        const answer = answers[index];
        if (error === undefined) answer?.resolve({});
        else answer?.reject(error);
      });
    },
  };
}

afterEach(() => {
  cleanup();
  vi.useRealTimers();
});

describe('useOptimisticMoves', () => {
  it('keeps an accepted move in place while a stale list still shows the old column', async () => {
    const t = setup();
    t.move('A', 'in_progress');
    expect(t.shown('A')).toBe('in_progress');
    expect([...t.hook.result.current.inFlight]).toEqual(['A']);

    await t.answer(0);
    expect(t.hook.result.current.inFlight.size).toBe(0);
    // A slow refresh that started before the move lands: no flip back.
    t.list('A', 'todo');
    expect(t.shown('A')).toBe('in_progress');
    // The event's refresh shows the move: the entry goes.
    t.list('A', 'in_progress');
    expect(t.shown('A')).toBe('in_progress');
    expect(t.hook.result.current.moves.size).toBe(0);
  });

  it('shows someone else’s later move', async () => {
    const t = setup();
    t.move('A', 'in_progress');
    await t.answer(0);
    t.list('A', 'done');
    expect(t.shown('A')).toBe('done');
    expect(t.hook.result.current.moves.size).toBe(0);
  });

  it('holds nothing when the event beat the answer', async () => {
    const t = setup();
    t.move('A', 'in_progress');
    t.list('A', 'in_progress');
    await t.answer(0);
    expect(t.hook.result.current.moves.size).toBe(0);
    expect(t.shown('A')).toBe('in_progress');
  });

  it('snaps refused moves back, one notice per task, cleared by a new move', async () => {
    const t = setup();
    t.move('A', 'done');
    t.move('B', 'done');
    await t.answer(0, new Error('no for A'));
    await t.answer(1, new Error('no for B'));
    expect(t.shown('A')).toBe('todo');
    expect(t.shown('B')).toBe('review');
    expect(t.hook.result.current.moves.size).toBe(0);
    expect([...t.hook.result.current.notices.values()].map((n) => n.text)).toEqual([
      'Couldn’t move P-A to Done: no for A',
      'Couldn’t move P-B to Done: no for B',
    ]);
    t.move('A', 'in_progress');
    expect([...t.hook.result.current.notices.keys()]).toEqual(['B']);
    act(() => t.hook.result.current.dismiss('B'));
    expect(t.hook.result.current.notices.size).toBe(0);
  });

  it('falls back to the list, without a notice, when it never catches up', async () => {
    vi.useFakeTimers({ toFake: ['setTimeout', 'clearTimeout'] });
    const t = setup(1_000);
    t.move('A', 'in_progress');
    await t.answer(0);
    act(() => vi.advanceTimersByTime(999));
    expect(t.shown('A')).toBe('in_progress');
    act(() => vi.advanceTimersByTime(1));
    expect(t.shown('A')).toBe('todo');
    expect(t.hook.result.current.moves.size).toBe(0);
    expect(t.hook.result.current.notices.size).toBe(0);
  });

  it('does not let an earlier move’s fallback undo a later one', async () => {
    vi.useFakeTimers({ toFake: ['setTimeout', 'clearTimeout'] });
    const t = setup(1_000);
    t.move('A', 'in_progress');
    await t.answer(0);
    act(() => vi.advanceTimersByTime(500));
    t.move('A', 'review');
    await t.answer(1);
    act(() => vi.advanceTimersByTime(600));
    expect(t.shown('A')).toBe('review');
    act(() => vi.advanceTimersByTime(500));
    expect(t.shown('A')).toBe('todo');
  });
});

describe('shownStatus and settle', () => {
  it('follow the rules above without React', () => {
    const a = task('A', 'todo');
    expect(shownStatus(a, undefined)).toBe('todo');
    expect(shownStatus(a, { id: 1, from: 'todo', to: 'done', accepted: false })).toBe('done');
    expect(shownStatus(a, { id: 1, from: 'todo', to: 'done', accepted: true })).toBe('done');
    expect(shownStatus({ ...a, status: 'review' }, { id: 1, from: 'todo', to: 'done', accepted: true })).toBe('review');
    const moves = new Map([['A', { id: 1, from: 'todo' as const, to: 'done' as const, accepted: true }]]);
    expect(settle(moves, [a])).toBe(moves);
    expect(settle(moves, [{ ...a, status: 'done' }]).size).toBe(0);
  });
});
