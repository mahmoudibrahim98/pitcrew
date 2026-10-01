// The task page: `tasks/$task`, reached by key or id. A page of its own (not a dialog), so a
// direct link, a reload or the palette's "jump to a task" land on something whole, with its own
// heading and focus target — see `e2e/shell.spec.ts`, "Ctrl K opens the palette and jumps to
// PAP-4", which this route must satisfy together with the shell.

import { useParams } from '@tanstack/react-router';
import type { ReactNode } from 'react';
import { useTasks } from './data.ts';
import { TaskDetail } from './task-drawer.tsx';

function H1({ className, children }: { className?: string; children?: ReactNode }) {
  return <h1 className={className}>{children}</h1>;
}

export function TaskPage() {
  const { task: ref }: { task?: string } = useParams({ strict: false });
  const tasks = useTasks();
  const task = tasks.data?.find((t) => t.key === ref || t.id === ref);

  if (tasks.data !== undefined && task === undefined) {
    return (
      <div className="mx-auto max-w-3xl px-6 py-6">
        <h1 className="text-xl font-semibold">Task not found</h1>
        <p className="mt-1 text-sm text-ink-2">Nothing lives at this address. It may have moved, or the link is wrong.</p>
      </div>
    );
  }
  if (task === undefined) {
    return (
      <div className="mx-auto max-w-3xl px-6 py-6">
        <h1 className="text-xl font-semibold">Loading…</h1>
      </div>
    );
  }
  return (
    <div className="mx-auto max-w-3xl px-6 py-6">
      <TaskDetail taskId={task.id} Title={H1} combineHeading />
    </div>
  );
}
