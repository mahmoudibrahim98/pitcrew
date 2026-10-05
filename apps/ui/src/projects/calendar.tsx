import { useRef, useState, type KeyboardEvent } from 'react';
import { Button } from '../design/index.ts';
import { useMe, useProjects, useTasks, useWorkstreams } from './data.ts';
import { dateValue, keyboardDay, monthDays, monthStart, shiftMonth, tasksByDay, todayDate, weekStart } from './calendar-dates.ts';
import { formatLongDay } from './format.ts';
import { ScheduledTask, useScheduledDrawer } from './scheduled-task.tsx';
import { TaskDrawer } from './task-drawer.tsx';
import { ErrorNote, Field, inputClass } from './ui.tsx';
import './schedule.css';

export function CalendarPage() {
  const today = todayDate();
  const [month, setMonth] = useState(() => monthStart(today));
  const [focused, setFocused] = useState(today);
  const [selected, setSelected] = useState<string>();
  const { taskId, openTask, onOpenChange } = useScheduledDrawer();
  const [mine, setMine] = useState(false);
  const [project, setProject] = useState('');
  const [workstream, setWorkstream] = useState('');
  const root = useRef<HTMLDivElement>(null);
  const me = useMe();
  const projects = useProjects();
  const workstreams = useWorkstreams(project || undefined);
  const tasks = useTasks({ ...(project === '' ? {} : { project }), ...(workstream === '' ? {} : { workstream }) });
  const filtered = (tasks.data ?? []).filter((task) => !task.archived && (!mine || (me.data !== undefined && task.assignee === me.data.id)));
  const byDay = tasksByDay(filtered);
  const days = monthDays(month, weekStart());
  const inMonth = days.filter((date) => date.slice(0, 7) === month.slice(0, 7));
  const agenda = inMonth.filter((date) => byDay.has(date));
  const title = new Intl.DateTimeFormat(undefined, { month: 'long', year: 'numeric', timeZone: 'UTC' }).format(dateValue(month));

  function changeMonth(next: string) {
    setMonth(monthStart(next));
    setFocused(next);
    setSelected(undefined);
  }

  function moveFocus(date: string, event: KeyboardEvent<HTMLButtonElement>) {
    const next = keyboardDay(date, event.key);
    if (next === undefined) return;
    event.preventDefault();
    setFocused(next);
    if (next.slice(0, 7) !== month.slice(0, 7)) setMonth(monthStart(next));
    requestAnimationFrame(() => root.current?.querySelector<HTMLButtonElement>(`[data-day="${next}"]`)?.focus());
  }

  return (
    <div className="schedule-page flex min-w-0 flex-col gap-4 px-4 py-6" ref={root}>
      <h1 className="text-2xl font-semibold">Calendar</h1>
      <div className="flex flex-wrap items-end gap-3">
        <label className="flex items-center gap-2 text-sm"><input type="checkbox" checked={mine} onChange={(event) => setMine(event.target.checked)} />Only my tasks</label>
        <Field label="Project">{(id) => <select id={id} className={inputClass} value={project} onChange={(event) => { setProject(event.target.value); setWorkstream(''); }}>
          <option value="">All projects</option>{projects.data?.map((item) => <option key={item.id} value={item.id}>{item.name}</option>)}
        </select>}</Field>
        <Field label="Workstream">{(id) => <select id={id} className={inputClass} value={workstream} onChange={(event) => setWorkstream(event.target.value)}>
          <option value="">All workstreams</option>{workstreams.data?.map((item) => <option key={item.id} value={item.id}>{item.name}</option>)}
        </select>}</Field>
      </div>
      {[tasks.error, projects.error, workstreams.error, mine ? me.error : null].map((error, index) => error === null ? null : <ErrorNote key={index} error={error} what="load the calendar" />)}
      <div className="flex flex-wrap items-center gap-2">
        <Button aria-label="Previous month" onClick={() => changeMonth(shiftMonth(month, -1))}>Previous</Button>
        <Button onClick={() => changeMonth(today)}>Today</Button>
        <Button aria-label="Next month" onClick={() => changeMonth(shiftMonth(month, 1))}>Next</Button>
        <h2 className="text-lg font-semibold" aria-live="polite">{title}</h2>
      </div>
      {tasks.data === undefined && tasks.error === null && <p role="status">Loading tasks…</p>}
      <p className="text-sm text-ink-2 calendar-help">Use arrow keys to move between days. Enter shows that day’s tasks.</p>
      <div className="calendar-month" role="group" aria-label={title}>
        {days.slice(0, 7).map((date) => <div key={date} className="calendar-weekday" aria-hidden="true">{new Intl.DateTimeFormat(undefined, { weekday: 'short', timeZone: 'UTC' }).format(dateValue(date))}</div>)}
        {days.map((date) => <div key={date} className="calendar-day" data-outside={date.slice(0, 7) !== month.slice(0, 7)}>
          <button type="button" data-day={date} tabIndex={focused === date ? 0 : -1} aria-current={date === today ? 'date' : undefined}
            aria-label={`${formatLongDay(date)}, ${byDay.get(date)?.length ?? 0} tasks`} aria-pressed={selected === date}
            onFocus={() => setFocused(date)} onKeyDown={(event) => moveFocus(date, event)} onClick={() => setSelected(date)}>
            {Number(date.slice(-2))}<span className="block text-xs">{byDay.get(date)?.length ?? 0} tasks</span>
          </button>
          {byDay.get(date)?.map((task) => <ScheduledTask key={task.id} task={task} onOpen={openTask} />)}
        </div>)}
      </div>
      <section className="calendar-agenda" aria-label="Days with tasks">
        {agenda.map((date) => <section key={date} aria-label={formatLongDay(date)}><h3 className="font-medium"><time dateTime={date}>{formatLongDay(date)}</time></h3>
          {byDay.get(date)?.map((task) => <ScheduledTask key={task.id} task={task} onOpen={openTask} />)}
        </section>)}
      </section>
      {selected !== undefined && <section className="calendar-selected" aria-label="Selected day tasks">
        <h3 className="font-medium">{formatLongDay(selected)}</h3>
        {byDay.get(selected)?.map((task) => <ScheduledTask key={task.id} task={task} onOpen={openTask} />)}
        {!byDay.has(selected) && <p className="text-sm text-ink-2">No tasks due this day.</p>}
      </section>}
      {tasks.data !== undefined && agenda.length === 0 && <p className="text-sm text-ink-2">No tasks due this month. Give a task a due date when creating it with + New → Task.</p>}
      {taskId !== undefined && <TaskDrawer taskId={taskId} open onOpenChange={onOpenChange} />}
    </div>
  );
}
