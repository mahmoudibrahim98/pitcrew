import { useState } from 'react';
import { Button, StatusPill } from '../design/index.ts';
import type { Task } from '../data/index.ts';
import { useMe, useTasks } from './data.ts';
import { Board } from './board.tsx';
import { useToday } from './calendar-dates.ts';
import { TASK_STATUS, PRIORITY } from './format.ts';
import { useProjectsNav } from './nav.tsx';
import { TaskDrawer } from './task-drawer.tsx';
import { ErrorNote } from './ui.tsx';

export const MY_TASK_GROUPS = ['Overdue', 'Today', 'Upcoming', 'No date', 'Completed'] as const;
export function myTaskGroup(task: Task, today: string): typeof MY_TASK_GROUPS[number] {
  if (task.status === 'done' || task.status === 'canceled') return 'Completed';
  if (task.due === undefined) return 'No date';
  return task.due < today ? 'Overdue' : task.due === today ? 'Today' : 'Upcoming';
}
function PersonTasks({ person }: { person: string }) {
  const preference = `pitcrew:my-tasks-view:${person}`;
  const [view, setView] = useState(() => {
    try { return localStorage.getItem(preference) === 'board' ? 'board' : 'list'; } catch { return 'list'; }
  });
  const [taskId, setTaskId] = useState<string | null>(null);
  const tasks = useTasks({ assignee: person });
  const nav = useProjectsNav();
  const today = useToday();
  const list = (tasks.data ?? []).filter((task) => !task.archived).sort((a, b) => (a.due ?? '').localeCompare(b.due ?? '') || PRIORITY[a.priority].rank - PRIORITY[b.priority].rank || a.key.localeCompare(b.key));
  return <>
    <div className="flex gap-2" role="group" aria-label="Task view">{['list', 'board'].map((value) => <Button key={value} aria-pressed={view === value} onClick={() => {
      setView(value); try { localStorage.setItem(preference, value); } catch { /* The current view still works without storage. */ }
    }}>{value === 'list' ? 'List' : 'Board'}</Button>)}</div>
    {view === 'board' ? <Board assignee={person} /> : <>
      {tasks.error !== null && <ErrorNote error={tasks.error} what="load your tasks" />}
      {tasks.data === undefined && tasks.error === null && <p role="status">Loading tasks…</p>}
      {MY_TASK_GROUPS.map((group) => <section key={group} aria-label={group} className="flex flex-col gap-2">
        <h2 className="text-lg font-semibold">{group}</h2>
        <ul className="flex flex-col gap-1">{list.filter((task) => myTaskGroup(task, today) === group).map((task) => <li key={task.id}>
          <button className="flex w-full items-center gap-2 rounded-md border border-line bg-card p-3 text-left text-sm" onClick={() => (nav.openTask ?? setTaskId)(task.id)}>
            <span className="font-mono text-xs text-ink-2">{task.key}</span><span>{task.title}</span>
            <StatusPill tone={TASK_STATUS[task.status].tone}>{TASK_STATUS[task.status].label}</StatusPill>
            {task.due !== undefined && <time className="ml-auto" dateTime={task.due}>{task.due}</time>}
          </button>
        </li>)}</ul>
      </section>)}
    </>}
    {taskId !== null && <TaskDrawer taskId={taskId} open onOpenChange={(open) => { if (!open) setTaskId(null); }} />}
  </>;
}

export function MyTasksPage() {
  const me = useMe();
  return (
    <div className="flex min-w-0 flex-col gap-4 px-6 py-6">
      <h1 className="text-2xl font-semibold">My tasks</h1>
      {me.error !== null && <ErrorNote error={me.error} what="load your profile" />}
      {me.data !== undefined && <PersonTasks key={me.data.id} person={me.data.id} />}
    </div>
  );
}
