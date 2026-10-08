import { useEffect, useState } from 'react';
import type { CalendarDate, Task } from '../data/index.ts';

const DAY_MS = 86_400_000;
export const dateValue = (date: CalendarDate): number => Date.parse(`${date}T00:00:00Z`);
export const dateKey = (value: number): CalendarDate => new Date(value).toISOString().slice(0, 10);
export const addDays = (date: CalendarDate, days: number): CalendarDate => dateKey(dateValue(date) + days * DAY_MS);

/** Today's calendar date in the viewer's location; arithmetic thereafter has no time zone. */
export function todayDate(now = new Date()): CalendarDate {
  return `${now.getFullYear()}-${String(now.getMonth() + 1).padStart(2, '0')}-${String(now.getDate()).padStart(2, '0')}`;
}

/** Milliseconds from `now` to the next local midnight. */
export function untilMidnight(now = new Date()): number {
  const next = new Date(now.getFullYear(), now.getMonth(), now.getDate() + 1);
  return next.getTime() - now.getTime();
}

/** Today's date, which turns over at local midnight while the page stays open. */
export function useToday(): CalendarDate {
  const [today, setToday] = useState(() => todayDate());
  useEffect(() => {
    // A second past midnight, so a timer that fires a little early still lands on the new day.
    const timer = setTimeout(() => setToday(todayDate()), untilMidnight() + 1000);
    return () => clearTimeout(timer);
  }, [today]);
  return today;
}

export function weekStart(locale = new Intl.DateTimeFormat().resolvedOptions().locale): number {
  const value = new Intl.Locale(locale) as Intl.Locale & {
    getWeekInfo?: () => { firstDay: number };
    weekInfo?: { firstDay: number };
  };
  return (value.getWeekInfo?.().firstDay ?? value.weekInfo?.firstDay ?? 1) % 7;
}

export function monthStart(date: CalendarDate): CalendarDate {
  return `${date.slice(0, 7)}-01`;
}

export function shiftMonth(date: CalendarDate, months: number): CalendarDate {
  const value = new Date(dateValue(monthStart(date)));
  value.setUTCMonth(value.getUTCMonth() + months);
  return dateKey(value.getTime());
}

export function monthDays(date: CalendarDate, firstDay = weekStart()): CalendarDate[] {
  const first = monthStart(date);
  const offset = (new Date(dateValue(first)).getUTCDay() - firstDay + 7) % 7;
  const end = shiftMonth(first, 1);
  const count = Math.ceil(((dateValue(end) - dateValue(first)) / DAY_MS + offset) / 7) * 7;
  return Array.from({ length: count }, (_, index) => addDays(first, index - offset));
}

export function keyboardDay(date: CalendarDate, key: string): CalendarDate | undefined {
  const steps: Record<string, number> = { ArrowLeft: -1, ArrowRight: 1, ArrowUp: -7, ArrowDown: 7 };
  const step = steps[key];
  return step === undefined ? undefined : addDays(date, step);
}

export function tasksByDay(tasks: readonly Task[]): Map<CalendarDate, Task[]> {
  const days = new Map<CalendarDate, Task[]>();
  for (const task of tasks) {
    if (task.due === undefined) continue;
    const entries = days.get(task.due) ?? [];
    entries.push(task);
    days.set(task.due, entries);
  }
  for (const entries of days.values()) entries.sort((a, b) => a.key.localeCompare(b.key, undefined, { numeric: true }));
  return days;
}

export type TimelineZoom = 'weeks' | 'months';

export function axisStart(date: CalendarDate, zoom: TimelineZoom, firstDay = weekStart()): CalendarDate {
  return zoom === 'months' ? monthStart(date) : addDays(date, -((new Date(dateValue(date)).getUTCDay() - firstDay + 7) % 7));
}

/** Include today and every due date, including overdue tasks. Undated tasks have no axis slot. */
export function timelineAxis(tasks: readonly Task[], today: CalendarDate, zoom: TimelineZoom, firstDay = weekStart()): CalendarDate[] {
  const dates = [today, ...tasks.flatMap((task) => task.due === undefined ? [] : [task.due])].sort();
  const start = axisStart(dates[0] ?? today, zoom, firstDay);
  const end = axisStart(dates.at(-1) ?? today, zoom, firstDay);
  const columns: CalendarDate[] = [];
  for (let date = start; date <= end; date = zoom === 'weeks' ? addDays(date, 7) : shiftMonth(date, 1)) columns.push(date);
  return columns;
}

export function axisPosition(date: CalendarDate | undefined, axis: readonly CalendarDate[], zoom: TimelineZoom, firstDay = weekStart()): number | undefined {
  if (date === undefined) return undefined;
  const index = axis.indexOf(axisStart(date, zoom, firstDay));
  return index < 0 ? undefined : index;
}

/** Fraction of a period before the day, used for the exact today line within its column. */
export function axisFraction(date: CalendarDate, zoom: TimelineZoom, firstDay = weekStart()): number {
  const start = axisStart(date, zoom, firstDay);
  const end = zoom === 'weeks' ? addDays(start, 7) : shiftMonth(start, 1);
  return (dateValue(date) - dateValue(start)) / (dateValue(end) - dateValue(start));
}
