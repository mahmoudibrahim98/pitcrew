// A task on the board: key, title, assignee, priority, due date, the agent's live status line, and
// "Needs you" when an open ask to me is about it.

import { useId, type KeyboardEvent, type PointerEvent } from 'react';
import { StatusPill } from '../design/index.ts';
import type { Member, Session, Task, TaskStatus } from '../data/index.ts';
import { cx } from '../lib/cx.ts';
import { PRIORITY, SESSION_STATE, TASK_STATUS, formatDay } from './format.ts';
import { Avatar } from './people.tsx';

export interface TaskCardProps {
  task: Task;
  /** Where the card shows now; ahead of the server while a move is in flight. */
  status: TaskStatus;
  assignee: Member | undefined;
  owner: Member | undefined;
  /** The agent's session on this task, if one is running. */
  session: Session | undefined;
  needsYou: boolean;
  /** Being dragged, or picked up with the keyboard. */
  lifted: boolean;
  /** Where a keyboard move would drop it. */
  target: TaskStatus | undefined;
  /** The server has not answered a move yet. */
  pending: boolean;
  instructionsId: string;
  /** One below the column's heading. */
  headingLevel: 4 | 5;
  onOpen: () => void;
  onHandleClick: () => void;
  onHandleKeyDown: (e: KeyboardEvent<HTMLButtonElement>) => void;
  onHandleBlur: () => void;
  onPointerDown: (e: PointerEvent<HTMLElement>) => void;
}

export function TaskCard({
  task,
  status,
  assignee,
  owner,
  session,
  needsYou,
  lifted,
  target,
  pending,
  instructionsId,
  headingLevel,
  onOpen,
  onHandleClick,
  onHandleKeyDown,
  onHandleBlur,
  onPointerDown,
}: TaskCardProps) {
  const titleId = useId();
  const priority = PRIORITY[task.priority];
  const Heading = headingLevel === 4 ? 'h4' : 'h5';
  return (
    <article
      aria-labelledby={titleId}
      aria-busy={pending || undefined}
      data-task={task.key}
      onPointerDown={onPointerDown}
      className={cx(
        'flex cursor-grab flex-col gap-1.5 rounded-md border bg-card p-2.5 select-none',
        lifted ? 'border-accent opacity-60' : 'border-line',
        pending && 'opacity-80',
      )}
    >
      <div className="flex flex-wrap items-center gap-x-2 gap-y-1 text-xs whitespace-nowrap">
        <span className="font-mono text-ink-2">{task.key}</span>
        {task.priority !== 'none' && (
          <span
            className={cx(
              'font-medium',
              priority.tone === 'risk' ? 'text-risk' : priority.tone === 'warn' ? 'text-warn' : 'text-ink-2',
            )}
          >
            <span className="sr-only">Priority: </span>
            {priority.label}
          </span>
        )}
        {needsYou && <span className="rounded-pill bg-warn-soft px-1.5 py-px font-medium text-warn">Needs you</span>}
        <span className="ml-auto">
          {task.assignee === undefined ? (
            <span className="text-ink-2">Unassigned</span>
          ) : (
            assignee !== undefined && <Avatar member={assignee} owner={owner} />
          )}
        </span>
      </div>
      <Heading id={titleId} className="text-sm leading-snug font-medium">
        <button type="button" onClick={onOpen} className="cursor-pointer rounded-sm text-left hover:underline">
          {task.title}
        </button>
      </Heading>
      {session !== undefined && (
        <p className="flex min-w-0 items-center gap-1.5 text-xs text-ink-2">
          <StatusPill tone={SESSION_STATE[session.state].tone}>{SESSION_STATE[session.state].label}</StatusPill>
          {session.status_line !== undefined && <span className="truncate">{session.status_line}</span>}
        </p>
      )}
      <div className="flex items-center gap-2 text-xs text-ink-2">
        {task.due !== undefined && <span>Due {formatDay(task.due)}</span>}
        {target !== undefined && (
          <span className="font-medium text-accent-text">→ {TASK_STATUS[target].label}</span>
        )}
        <button
          type="button"
          data-move-handle={task.id}
          aria-label={`Move ${task.key}, now ${TASK_STATUS[status].label}`}
          aria-describedby={instructionsId}
          aria-pressed={target !== undefined}
          onClick={onHandleClick}
          onKeyDown={onHandleKeyDown}
          onBlur={onHandleBlur}
          className="ml-auto inline-flex size-6 cursor-pointer items-center justify-center rounded-sm text-ink-2 hover:bg-hover hover:text-ink"
        >
          <span aria-hidden>⇄</span>
        </button>
      </div>
    </article>
  );
}
