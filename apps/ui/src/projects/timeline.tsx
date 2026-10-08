import { useState, type CSSProperties } from 'react';
import { Button } from '../design/index.ts';
import { useTasks, useWorkstreams } from './data.ts';
import { axisFraction, axisPosition, timelineAxis, todayDate, weekStart, type TimelineZoom } from './calendar-dates.ts';
import { formatDay } from './format.ts';
import { ScheduledTask, useScheduledDrawer } from './scheduled-task.tsx';
import { TaskDrawer } from './task-drawer.tsx';
import { ErrorNote } from './ui.tsx';
import './schedule.css';

export function Timeline({ project }: { project: string }) {
  const [zoom, setZoom] = useState<TimelineZoom>('weeks');
  const { taskId, openTask, onOpenChange } = useScheduledDrawer();
  const tasks = useTasks({ project });
  const workstreams = useWorkstreams(project);
  const today = todayDate();
  const firstDay = weekStart();
  const axis = timelineAxis(tasks.data ?? [], today, zoom, firstDay);
  const positions = new Map((tasks.data ?? []).filter((task) => !task.archived).map((task) => [task.id, axisPosition(task.due, axis, zoom, firstDay)]));
  const todayColumn = axisPosition(today, axis, zoom, firstDay);
  const known = new Set(workstreams.data?.map((item) => item.id));
  const rows = [...(workstreams.data ?? []).map((item) => ({ id: item.id, name: item.name })), { id: '', name: 'Outside a workstream' }];
  const rowTasks = (id: string) => (tasks.data ?? []).filter((task) => !task.archived).filter((task) => id === '' ? task.workstream === undefined || !known.has(task.workstream) : task.workstream === id);

  return (
    <section className="schedule-page flex min-w-0 max-w-full flex-col gap-3" aria-label="Project timeline">
      <div className="flex flex-wrap items-center gap-2">
        <h2 className="text-lg font-semibold">Timeline</h2>
        <div role="group" aria-label="Timeline zoom">
          <Button aria-pressed={zoom === 'weeks'} onClick={() => setZoom('weeks')}>Weeks</Button>{' '}
          <Button aria-pressed={zoom === 'months'} onClick={() => setZoom('months')}>Months</Button>
        </div>
      </div>
      <p className="text-sm text-ink-2">○ Open · ◐ In progress · ◇ Review · ● Done · × Canceled. Tasks show their exact due date.</p>
      {tasks.error !== null && <ErrorNote error={tasks.error} what="load timeline tasks" />}
      {workstreams.error !== null && <ErrorNote error={workstreams.error} what="load workstreams" />}
      {tasks.data === undefined && tasks.error === null && <p role="status">Loading tasks…</p>}
      <div className="timeline-scroll" role="region" aria-label="Timeline time axis" tabIndex={0}>
        <table className="timeline-table" style={{ '--today-position': `${axisFraction(today, zoom, firstDay) * 100}%` } as CSSProperties}>
          <caption className="sr-only">Tasks by workstream and due {zoom === 'weeks' ? 'week' : 'month'}</caption>
          <thead><tr><th scope="col">Workstream</th>{axis.map((date, index) => <th scope="col" key={date} data-today={index === todayColumn}>
            {zoom === 'weeks' ? `Week of ${formatDay(date)}` : new Intl.DateTimeFormat(undefined, { month: 'short', year: 'numeric', timeZone: 'UTC' }).format(new Date(`${date}T00:00:00Z`))}
            {index === todayColumn && <span className="block">Today · {formatDay(today)}</span>}
          </th>)}</tr></thead>
          <tbody>{rows.map((row) => <tr key={row.id}><th scope="row">{row.name}</th>{axis.map((date, index) => <td key={date} data-today={index === todayColumn}>
            {rowTasks(row.id).filter((task) => positions.get(task.id) === index).map((task) => <ScheduledTask key={task.id} task={task} onOpen={openTask} />)}
          </td>)}</tr>)}</tbody>
        </table>
      </div>
      <section aria-label="Tasks without a due date"><h3 className="font-medium">No due date</h3>
        {rows.map((row) => {
          const undated = rowTasks(row.id).filter((task) => task.due === undefined);
          return undated.length === 0 ? null : <section key={row.id} aria-label={`${row.name}, no due date`}><h4 className="text-sm text-ink-2">{row.name}</h4>
            {undated.map((task) => <ScheduledTask key={task.id} task={task} onOpen={openTask} />)}
          </section>;
        })}
        {tasks.data?.every((task) => task.due !== undefined) && <p className="text-sm text-ink-2">Every task has a due date.</p>}
      </section>
      {taskId !== undefined && <TaskDrawer taskId={taskId} open onOpenChange={onOpenChange} />}
    </section>
  );
}
