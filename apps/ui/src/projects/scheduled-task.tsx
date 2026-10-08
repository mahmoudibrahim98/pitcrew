import type { Task } from '../data/index.ts';
import { useRef, useState } from 'react';
import { TASK_STATUS, formatDay } from './format.ts';
import { useProjectsNav } from './nav.tsx';

const SHAPES = { backlog: '○', todo: '○', in_progress: '◐', review: '◇', done: '●', canceled: '×' };

/**
 * Opens a task from the calendar or timeline: in the projects layout's one drawer when there is
 * one (so a task opened from inside it replaces it rather than stacking a second drawer), else
 * in a drawer of the page's own.
 */
export function useScheduledDrawer() {
  const nav = useProjectsNav();
  const [taskId, setTaskId] = useState<string>();
  const trigger = useRef<HTMLElement | null>(null);
  return {
    taskId,
    openTask: (id: string) => {
      if (nav.openTask !== undefined) {
        nav.openTask(id);
        return;
      }
      trigger.current = document.activeElement instanceof HTMLElement ? document.activeElement : null;
      setTaskId(id);
    },
    onOpenChange: (open: boolean) => {
      if (open) return;
      setTaskId(undefined);
      requestAnimationFrame(() => { if (trigger.current?.isConnected) trigger.current.focus(); });
    },
  };
}

export function ScheduledTask({ task, onOpen }: { task: Task; onOpen: (id: string) => void }) {
  return (
    <button type="button" data-scheduled-task={task.id} data-status={task.status}
      className="scheduled-task" onClick={() => onOpen(task.id)}>
      <span aria-hidden="true">{SHAPES[task.status]}</span>{' '}
      {task.key} · {task.title}
      <span className="block text-xs">{TASK_STATUS[task.status].label}{task.due === undefined ? '' : ` · ${formatDay(task.due)}`}</span>
    </button>
  );
}
