// @vitest-environment happy-dom
import { act, renderHook } from '@testing-library/react';
import { afterEach, describe, expect, it, vi } from 'vitest';
import type { Task } from '../../data/index.ts';
import { addDays, axisFraction, axisPosition, keyboardDay, monthDays, shiftMonth, tasksByDay, timelineAxis, todayDate, untilMidnight, useToday, weekStart } from '../calendar-dates.ts';

const task = (id: string, due?: string): Task => ({
  id, key: `PAP-${id}`, project: 'paper', title: id, description: '', status: 'todo', priority: 'none',
  labels: [], blocked_by: [], accept_auto: false, subtasks: [], ...(due === undefined ? {} : { due }),
});

describe('calendar dates', () => {
  it('fills whole locale weeks across month and year edges', () => {
    const days = monthDays('2026-01-01', 1);
    expect(days[0]).toBe('2025-12-29');
    expect(days.at(-1)).toBe('2026-02-01');
    expect(days.length % 7).toBe(0);
    expect(monthDays('2024-02-01', 0)).toContain('2024-02-29');
    expect(shiftMonth('2024-01-31', 1)).toBe('2024-02-01');
    expect(shiftMonth('2026-12-15', 1)).toBe('2027-01-01');
  });

  it('keeps calendar dates stable across leap days and daylight saving', () => {
    expect(addDays('2024-02-28', 1)).toBe('2024-02-29');
    expect(addDays('2024-02-29', 1)).toBe('2024-03-01');
    expect(addDays('2026-03-29', 1)).toBe('2026-03-30');
    expect(todayDate(new Date(2026, 9, 1, 23, 59))).toBe('2026-10-01');
  });

  it('uses the locale first day where Intl exposes it', () => {
    expect(weekStart('en-US')).toBe(0);
    expect(weekStart('en-GB')).toBe(1);
  });

  it('groups tasks by due date, sorts keys and leaves undated tasks apart', () => {
    const a = task('10', '2024-02-29');
    const b = task('2', '2024-02-29');
    const days = tasksByDay([a, task('1'), b, task('3', '2024-03-01')]);
    expect(days.get('2024-02-29')).toEqual([b, a]);
    expect([...days.keys()]).toEqual(['2024-02-29', '2024-03-01']);
  });

  it.each([
    ['ArrowLeft', '2024-02-29'], ['ArrowRight', '2024-03-02'],
    ['ArrowUp', '2024-02-23'], ['ArrowDown', '2024-03-08'],
  ])('moves a day with %s', (key, date) => {
    expect(keyboardDay('2024-03-01', key)).toBe(date);
  });

  it('leaves Enter and unrelated keys to the day button', () => {
    expect(keyboardDay('2024-03-01', 'Enter')).toBeUndefined();
    expect(keyboardDay('2024-03-01', 'Tab')).toBeUndefined();
  });
});

describe('timeline placement', () => {
  const tasks = [task('1', '2024-02-29'), task('2', '2024-03-01'), task('3')];
  it('places month edges, leap day and today on one axis', () => {
    const axis = timelineAxis(tasks, '2024-04-01', 'months', 1);
    expect(axis).toEqual(['2024-02-01', '2024-03-01', '2024-04-01']);
    expect(axisPosition(tasks[0]?.due, axis, 'months', 1)).toBe(0);
    expect(axisPosition(tasks[1]?.due, axis, 'months', 1)).toBe(1);
    expect(axisPosition(tasks[2]?.due, axis, 'months', 1)).toBeUndefined();
    expect(axisFraction('2024-02-29', 'months', 1)).toBeCloseTo(28 / 29);
    expect(axisFraction('2024-03-01', 'weeks', 1)).toBeCloseTo(4 / 7);
  });
  it('places tasks on locale weeks and includes overdue tasks', () => {
    const axis = timelineAxis(tasks, '2024-03-04', 'weeks', 1);
    expect(axis).toEqual(['2024-02-26', '2024-03-04']);
    expect(axisPosition('2024-03-01', axis, 'weeks', 1)).toBe(0);
    expect(axisPosition('2024-03-04', axis, 'weeks', 1)).toBe(1);
    expect(timelineAxis([], '2024-03-04', 'weeks', 0)).toEqual(['2024-03-03']);
  });
});

describe('useToday', () => {
  afterEach(() => {
    vi.useRealTimers();
  });

  it('turns over at the next local midnight while the page stays open', () => {
    vi.useFakeTimers({ now: new Date(2026, 9, 1, 23, 59, 30) });
    expect(untilMidnight()).toBe(30_000);
    const { result } = renderHook(() => useToday());
    expect(result.current).toBe('2026-10-01');
    act(() => vi.advanceTimersByTime(29_000));
    expect(result.current).toBe('2026-10-01');
    act(() => vi.advanceTimersByTime(2_000));
    expect(result.current).toBe('2026-10-02');
    // And again the night after.
    act(() => vi.advanceTimersByTime(86_400_000));
    expect(result.current).toBe('2026-10-03');
  });
});
