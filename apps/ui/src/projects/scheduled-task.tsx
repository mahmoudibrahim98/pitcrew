import type { Task } from '../data/index.ts';
import { useRef, useState } from 'react';
import { TASK_STATUS, formatDay } from './format.ts';

const SHAPES = { backlog: '○', todo: '○', in_progress: '◐', review: '◇', done: '●', canceled: '×' };

export function useScheduledDrawer() {
  const [taskId, setTaskId] = useState<string>();
  const trigger = useRef<HTMLElement | null>(null);
  return {
    taskId,
    openTask: (id: string) => {
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
