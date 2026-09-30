// The board: tasks in columns by status, optionally in one lane per workstream. Dragging a card, or
// picking it up with the keyboard, asks the hub to move it; the card shows in its new column while
// the request is out, and snaps back with the server's message if the hub says no.

import { useQueryClient } from '@tanstack/react-query';
import { ToggleGroup } from 'radix-ui';
import {
  useEffect,
  useId,
  useRef,
  useState,
  type FormEvent,
  type KeyboardEvent,
  type PointerEvent,
  type ReactNode,
} from 'react';
import { Button } from '../design/index.ts';
import {
  keys,
  type Member,
  type MemberId,
  type ProjectId,
  type Session,
  type Task,
  type TaskFilters,
  type TaskId,
  type TaskStatus,
  type WorkstreamId,
} from '../data/index.ts';
import { cx } from '../lib/cx.ts';
import {
  useCreateTask,
  useInbox,
  useMemberMap,
  useMoveTask,
  useOptionalWorkstream,
  useSessions,
  useTasks,
  useWorkstreams,
} from './data.ts';
import { PRIORITY, SESSION_STATE, STATUS_ORDER, TASK_STATUS, shortId } from './format.ts';
import { useProjectsNav } from './nav.tsx';
import { TaskCard } from './task-card.tsx';
import { TaskDrawer } from './task-drawer.tsx';
import { ErrorNote, VisuallyHidden, inputClass } from './ui.tsx';
import { CardList } from './virtual-list.tsx';

export type BoardGrouping = 'status' | 'workstream';

export interface Lane {
  id: string;
  title?: string;
}

const NO_WORKSTREAM = 'none';

/** How long an accepted move outranks list refreshes that still show the old status. */
const SETTLE_MS = 3_000;

interface PendingMove {
  from: TaskStatus;
  to: TaskStatus;
  /** The list's `dataUpdatedAt` when the hub accepted the move; absent while in flight. */
  acceptedAt?: number;
}

function keyNumber(key: string): number {
  const n = Number(key.slice(key.lastIndexOf('-') + 1));
  return Number.isFinite(n) ? n : 0;
}

function byPriorityThenKey(a: Task, b: Task): number {
  return PRIORITY[a.priority].rank - PRIORITY[b.priority].rank || keyNumber(a.key) - keyNumber(b.key);
}

/** The session to show on a task's card: the liveliest, then the most recent. */
export function sessionsByTask(sessions: readonly Session[]): Map<TaskId, Session> {
  const best = new Map<TaskId, Session>();
  for (const session of sessions) {
    if (session.task === undefined || session.state === 'ended') continue;
    const current = best.get(session.task);
    const better =
      current === undefined ||
      SESSION_STATE[session.state].rank < SESSION_STATE[current.state].rank ||
      (session.state === current.state && session.last_activity > current.last_activity);
    if (better) best.set(session.task, session);
  }
  return best;
}

export interface BoardProps {
  project?: ProjectId;
  workstream?: WorkstreamId;
  /** Initial grouping; people can switch it. */
  groupBy?: BoardGrouping;
  title?: string;
  /** Where a card leads. Without one, the board opens its own task drawer. */
  onOpenTask?: (task: TaskId) => void;
}

export function Board({ project, workstream, groupBy = 'status', title = 'Board', onOpenTask }: BoardProps) {
  const filters: TaskFilters = {};
  if (project !== undefined) filters.project = project;
  if (workstream !== undefined) filters.workstream = workstream;

  const headingId = useId();
  const queryClient = useQueryClient();
  const nav = useProjectsNav();
  const tasks = useTasks(filters);
  const sessions = useSessions();
  const inbox = useInbox();
  const members = useMemberMap();
  const workstreams = useWorkstreams(project);
  const parent = useOptionalWorkstream(project === undefined ? workstream : undefined);
  const move = useMoveTask();
  const create = useCreateTask();

  const [grouping, setGrouping] = useState<BoardGrouping>(groupBy);
  const [moves, setMoves] = useState<ReadonlyMap<TaskId, PendingMove>>(new Map());
  const [notice, setNotice] = useState<string | null>(null);
  const [drawerTask, setDrawerTask] = useState<TaskId | null>(null);

  const statusOf = (task: Task): TaskStatus => {
    const pending = moves.get(task.id);
    if (pending === undefined) return task.status;
    if (pending.acceptedAt === undefined) return pending.to;
    const catchingUp = task.status === pending.from && tasks.dataUpdatedAt - pending.acceptedAt < SETTLE_MS;
    return catchingUp ? pending.to : task.status;
  };

  const requestMove = (task: Task, to: TaskStatus) => {
    if (statusOf(task) === to) return;
    const from = task.status;
    setNotice(null);
    setMoves((m) => new Map(m).set(task.id, { from, to }));
    // `mutateAsync`, not `mutate`: per-call callbacks of `mutate` fire only for the latest call, and
    // several cards can be in flight at once.
    move.mutateAsync({ task: task.id, to }).then(
      () => {
        const acceptedAt = queryClient.getQueryState(keys.tasks.list(filters))?.dataUpdatedAt ?? 0;
        setMoves((m) => new Map(m).set(task.id, { from, to, acceptedAt }));
      },
      (error: unknown) => {
        setMoves((m) => {
          const next = new Map(m);
          next.delete(task.id);
          return next;
        });
        const reason = error instanceof Error ? error.message : String(error);
        setNotice(`Couldn’t move ${task.key} to ${TASK_STATUS[to].label}: ${reason}`);
      },
    );
  };

  const workstreamNames = new Map((workstreams.data ?? []).map((w) => [w.id, w.name]));
  const list = tasks.data ?? [];
  const lanes: Lane[] = [];
  if (grouping === 'status') {
    lanes.push({ id: 'all' });
  } else {
    const ids = new Set(list.map((t) => t.workstream ?? NO_WORKSTREAM));
    const ordered = [
      ...(workstreams.data ?? []).map((w) => w.id).filter((w) => ids.has(w)),
      ...[...ids].filter((w) => w !== NO_WORKSTREAM && !workstreamNames.has(w)),
    ];
    for (const w of ordered) lanes.push({ id: w, title: workstreamNames.get(w) ?? shortId(w) });
    if (ids.has(NO_WORKSTREAM)) lanes.push({ id: NO_WORKSTREAM, title: 'No workstream' });
  }

  const projectId = project ?? parent.data?.project;
  const createIn =
    projectId === undefined
      ? undefined
      : (status: TaskStatus, lane: string, text: string, done: () => void) => {
          const inWorkstream = lane !== 'all' && lane !== NO_WORKSTREAM ? lane : workstream;
          create.mutate(
            inWorkstream === undefined
              ? { project: projectId, title: text, status }
              : { project: projectId, workstream: inWorkstream, title: text, status },
            { onSuccess: done },
          );
        };

  const openTask = onOpenTask ?? nav.openTask;
  const open = (task: Task) => {
    if (openTask !== undefined) openTask(task.id);
    else setDrawerTask(task.id);
  };

  const pending = new Set([...moves].filter(([, m]) => m.acceptedAt === undefined).map(([id]) => id));
  const needsYou = new Set((inbox.data ?? []).flatMap((ask) => (ask.task === undefined ? [] : [ask.task])));

  return (
    <section aria-labelledby={headingId} className="flex min-w-0 flex-col gap-3">
      <header className="flex flex-wrap items-center gap-3">
        <h2 id={headingId} className="text-lg font-semibold">
          {title}
        </h2>
        <ToggleGroup.Root
          type="single"
          value={grouping}
          onValueChange={(value) => value !== '' && setGrouping(value as BoardGrouping)}
          aria-label="Group by"
          className="ml-auto inline-flex rounded-sm border border-line bg-sunken p-0.5"
        >
          <ToggleGroup.Item value="status" className={TOGGLE_ITEM}>
            By status
          </ToggleGroup.Item>
          <ToggleGroup.Item value="workstream" className={TOGGLE_ITEM}>
            By workstream
          </ToggleGroup.Item>
        </ToggleGroup.Root>
      </header>

      {notice !== null && (
        <div role="alert" className="flex items-start gap-2 rounded-sm border border-risk bg-risk-soft p-2 text-sm text-risk">
          <p className="flex-1">{notice}</p>
          <Button variant="ghost" onClick={() => setNotice(null)}>
            Dismiss
          </Button>
        </div>
      )}
      {tasks.error !== null && <ErrorNote error={tasks.error} what="load the tasks" />}
      {create.error !== null && <ErrorNote error={create.error} what="add the task" />}
      {tasks.isPending && tasks.error === null && <p className="text-sm text-ink-2">Loading tasks…</p>}

      {tasks.data !== undefined && (
        <BoardView
          tasks={list}
          statusOf={statusOf}
          pending={pending}
          sessions={sessionsByTask(sessions.data ?? [])}
          needsYou={needsYou}
          members={members}
          lanes={lanes}
          laneOf={(task) => (grouping === 'status' ? 'all' : (task.workstream ?? NO_WORKSTREAM))}
          onMove={requestMove}
          onOpen={open}
          {...(createIn === undefined ? {} : { onCreate: createIn })}
        />
      )}

      {drawerTask !== null && (
        <TaskDrawer taskId={drawerTask} open onOpenChange={(isOpen) => !isOpen && setDrawerTask(null)} />
      )}
    </section>
  );
}

const TOGGLE_ITEM =
  'h-6 rounded-sm px-2 text-xs text-ink-2 data-[state=on]:bg-card data-[state=on]:text-ink data-[state=on]:shadow-sm';

interface Drag {
  task: TaskId;
  lane: string;
  x: number;
  y: number;
  active: boolean;
}

export interface BoardViewProps {
  tasks: readonly Task[];
  statusOf: (task: Task) => TaskStatus;
  pending: ReadonlySet<TaskId>;
  sessions: ReadonlyMap<TaskId, Session>;
  needsYou: ReadonlySet<TaskId>;
  members: ReadonlyMap<MemberId, Member>;
  lanes: readonly Lane[];
  laneOf: (task: Task) => string;
  onMove: (task: Task, to: TaskStatus) => void;
  onOpen: (task: Task) => void;
  onCreate?: (status: TaskStatus, lane: string, title: string, done: () => void) => void;
}

const cellKey = (lane: string, status: TaskStatus) => `${lane}/${status}`;

/** The board without data: lanes, columns, cards, dragging and the keyboard alternative. */
export function BoardView({
  tasks,
  statusOf,
  pending,
  sessions,
  needsYou,
  members,
  lanes,
  laneOf,
  onMove,
  onOpen,
  onCreate,
}: BoardViewProps) {
  const instructionsId = useId();
  const root = useRef<HTMLDivElement>(null);
  const [drag, setDrag] = useState<Drag | null>(null);
  const [over, setOver] = useState<string | null>(null);
  const [picked, setPicked] = useState<{ task: TaskId; target: TaskStatus } | null>(null);
  const [focusTask, setFocusTask] = useState<TaskId | null>(null);
  const [announcement, setAnnouncement] = useState('');
  const [adding, setAdding] = useState<string | null>(null);

  const columns = STATUS_ORDER.filter((s) => s !== 'canceled' || tasks.some((t) => statusOf(t) === 'canceled'));
  const cells = new Map<string, Task[]>();
  for (const task of tasks) {
    const key = cellKey(laneOf(task), statusOf(task));
    const cell = cells.get(key);
    if (cell === undefined) cells.set(key, [task]);
    else cell.push(task);
  }
  for (const cell of cells.values()) cell.sort(byPriorityThenKey);
  const find = (id: TaskId) => tasks.find((t) => t.id === id);

  // Keep keyboard focus on a card that moved to another column (its old element is gone).
  const focusStatus = focusTask === null ? undefined : (() => {
    const task = find(focusTask);
    return task === undefined ? undefined : statusOf(task);
  })();
  useEffect(() => {
    if (focusTask === null) return;
    const active = document.activeElement;
    if (active !== null && active !== document.body) return;
    root.current?.querySelector<HTMLElement>(`[data-move-handle="${focusTask}"]`)?.focus();
  }, [focusTask, focusStatus]);

  // A drag ends wherever the pointer is released; outside a column, or on Escape, nothing moves.
  const dragging = drag !== null;
  useEffect(() => {
    if (!dragging) return;
    const cancel = () => {
      setDrag(null);
      setOver(null);
    };
    const onKey = (e: globalThis.KeyboardEvent) => {
      if (e.key === 'Escape') cancel();
    };
    window.addEventListener('pointerup', cancel);
    window.addEventListener('keydown', onKey);
    window.addEventListener('blur', cancel);
    return () => {
      window.removeEventListener('pointerup', cancel);
      window.removeEventListener('keydown', onKey);
      window.removeEventListener('blur', cancel);
    };
  }, [dragging]);

  const startDrag = (task: Task, lane: string) => (e: PointerEvent<HTMLElement>) => {
    if (e.button !== 0 || picked !== null) return;
    if (e.target instanceof Element && e.target.closest('input, textarea, select, a') !== null) return;
    setDrag({ task: task.id, lane, x: e.clientX, y: e.clientY, active: false });
  };

  const onPointerMove = (e: PointerEvent<HTMLElement>) => {
    if (drag === null || drag.active) return;
    if (Math.hypot(e.clientX - drag.x, e.clientY - drag.y) >= 4) setDrag({ ...drag, active: true });
  };

  const hover = (lane: string, status: TaskStatus) => () => {
    if (drag?.active === true && drag.lane === lane) setOver(cellKey(lane, status));
  };

  const drop = (lane: string, status: TaskStatus) => () => {
    if (drag?.active !== true || drag.lane !== lane) return;
    const task = find(drag.task);
    setDrag(null);
    setOver(null);
    if (task !== undefined && statusOf(task) !== status) onMove(task, status);
  };

  const handleClick = (task: Task) => () => {
    const status = statusOf(task);
    if (picked?.task !== task.id) {
      setPicked({ task: task.id, target: status });
      setAnnouncement(
        `Picked up ${task.key} in ${TASK_STATUS[status].label}. Left and right arrows choose a column; Enter or Space drops it; Escape cancels.`,
      );
      return;
    }
    const to = picked.target;
    setPicked(null);
    if (to === status) {
      setAnnouncement(`${task.key} stays in ${TASK_STATUS[status].label}.`);
      return;
    }
    setFocusTask(task.id);
    setAnnouncement(`Moving ${task.key} to ${TASK_STATUS[to].label}.`);
    onMove(task, to);
  };

  const handleBlur = (task: Task) => () => {
    if (picked?.task !== task.id) return;
    setPicked(null);
    setAnnouncement(`Move of ${task.key} canceled.`);
  };

  const handleKeyDown = (task: Task) => (e: KeyboardEvent<HTMLButtonElement>) => {
    if (picked?.task !== task.id) return;
    if (e.key === 'ArrowRight' || e.key === 'ArrowLeft') {
      e.preventDefault();
      const at = columns.indexOf(picked.target);
      const next = columns[Math.min(columns.length - 1, Math.max(0, at + (e.key === 'ArrowRight' ? 1 : -1)))];
      if (next === undefined) return;
      setPicked({ task: task.id, target: next });
      setAnnouncement(`${task.key}: ${TASK_STATUS[next].label}.`);
    } else if (e.key === 'Escape') {
      e.preventDefault();
      setPicked(null);
      setAnnouncement(`Move of ${task.key} canceled.`);
    }
  };

  const renderCard = (lane: string) => (task: Task) => {
    const assignee = task.assignee === undefined ? undefined : members.get(task.assignee);
    const owner = assignee?.owner === undefined ? undefined : members.get(assignee.owner);
    return (
      <TaskCard
        task={task}
        status={statusOf(task)}
        assignee={assignee}
        owner={owner}
        session={sessions.get(task.id)}
        needsYou={needsYou.has(task.id)}
        lifted={(drag?.active === true && drag.task === task.id) || picked?.task === task.id}
        target={picked?.task === task.id ? picked.target : undefined}
        pending={pending.has(task.id)}
        instructionsId={instructionsId}
        onOpen={() => onOpen(task)}
        onHandleClick={handleClick(task)}
        onHandleKeyDown={handleKeyDown(task)}
        onHandleBlur={handleBlur(task)}
        onPointerDown={startDrag(task, lane)}
      />
    );
  };

  const laneHeading = lanes.length > 1 || lanes[0]?.title !== undefined;
  const pickedTask = picked === null ? undefined : find(picked.task);

  return (
    <div
      ref={root}
      onPointerMove={onPointerMove}
      className={cx('flex flex-col gap-4', drag?.active === true && 'cursor-grabbing select-none')}
    >
      <VisuallyHidden id={instructionsId}>
        Enter or Space picks the task up; the left and right arrow keys choose a column; Enter or Space
        drops it there; Escape cancels.
      </VisuallyHidden>
      <p role="status" aria-live="polite" className="sr-only">
        {announcement}
      </p>
      {lanes.map((lane) => {
        const grid = (
          <div className="grid auto-cols-[minmax(15rem,1fr)] grid-flow-col gap-3 overflow-x-auto pb-2">
            {columns.map((status) => {
              const key = cellKey(lane.id, status);
              const cards = cells.get(key) ?? [];
              const targeted =
                over === key ||
                (pickedTask !== undefined &&
                  picked?.target === status &&
                  laneOf(pickedTask) === lane.id &&
                  statusOf(pickedTask) !== status);
              return (
                <Column
                  key={key}
                  status={status}
                  count={cards.length}
                  level={laneHeading ? 4 : 3}
                  targeted={targeted}
                  onPointerEnter={hover(lane.id, status)}
                  onPointerMove={hover(lane.id, status)}
                  onPointerUp={drop(lane.id, status)}
                  adding={adding === key}
                  onAdd={onCreate === undefined ? undefined : () => setAdding(key)}
                  onCancelAdd={() => setAdding(null)}
                  onSubmitAdd={
                    onCreate === undefined
                      ? undefined
                      : (title) => onCreate(status, lane.id, title, () => setAdding(null))
                  }
                >
                  <CardList items={cards} itemKey={(t) => t.id} renderItem={renderCard(lane.id)} />
                </Column>
              );
            })}
          </div>
        );
        if (!laneHeading) return <div key={lane.id}>{grid}</div>;
        return <LaneSection key={lane.id} title={lane.title ?? shortId(lane.id)}>{grid}</LaneSection>;
      })}
    </div>
  );
}

function LaneSection({ title, children }: { title: string; children: ReactNode }) {
  const id = useId();
  return (
    <section aria-labelledby={id} className="flex flex-col gap-2">
      <h3 id={id} className="text-md font-semibold">
        {title}
      </h3>
      {children}
    </section>
  );
}

function Column({
  status,
  count,
  level,
  targeted,
  onPointerEnter,
  onPointerMove,
  onPointerUp,
  adding,
  onAdd,
  onCancelAdd,
  onSubmitAdd,
  children,
}: {
  status: TaskStatus;
  count: number;
  level: 3 | 4;
  targeted: boolean;
  onPointerEnter: () => void;
  onPointerMove: () => void;
  onPointerUp: () => void;
  adding: boolean;
  onAdd: (() => void) | undefined;
  onCancelAdd: () => void;
  onSubmitAdd: ((title: string) => void) | undefined;
  children: ReactNode;
}) {
  const headingId = useId();
  const Heading = level === 3 ? 'h3' : 'h4';
  const label = TASK_STATUS[status].label;
  return (
    <div
      role="group"
      aria-labelledby={headingId}
      data-status={status}
      onPointerEnter={onPointerEnter}
      onPointerMove={onPointerMove}
      onPointerUp={onPointerUp}
      className={cx(
        'flex min-w-0 flex-col gap-2 rounded-md bg-sunken p-2 transition-shadow',
        targeted && 'ring-2 ring-accent',
      )}
    >
      <div className="flex items-center gap-2 px-1">
        <Heading id={headingId} className="text-sm font-semibold">
          {label}
        </Heading>
        <span className="text-xs text-ink-2">
          {count}
          <span className="sr-only"> tasks</span>
        </span>
        {onAdd !== undefined && !adding && (
          <button
            type="button"
            onClick={onAdd}
            aria-label={`Add a task to ${label}`}
            className="ml-auto inline-flex size-6 items-center justify-center rounded-sm text-ink-2 hover:bg-hover hover:text-ink"
          >
            <span aria-hidden>+</span>
          </button>
        )}
      </div>
      {adding && onSubmitAdd !== undefined && <AddTask label={label} onSubmit={onSubmitAdd} onCancel={onCancelAdd} />}
      {children}
    </div>
  );
}

function AddTask({ label, onSubmit, onCancel }: { label: string; onSubmit: (title: string) => void; onCancel: () => void }) {
  const [title, setTitle] = useState('');
  const submit = (e: FormEvent) => {
    e.preventDefault();
    if (title.trim() !== '') onSubmit(title.trim());
  };
  return (
    <form onSubmit={submit} className="flex flex-col gap-1.5">
      <input
        aria-label={`New task in ${label}`}
        autoFocus
        value={title}
        placeholder="Title"
        onChange={(e) => setTitle(e.target.value)}
        onKeyDown={(e) => e.key === 'Escape' && onCancel()}
        className={inputClass}
      />
      <div className="flex gap-1.5">
        <Button type="submit" variant="primary">
          Add
        </Button>
        <Button onClick={onCancel}>Cancel</Button>
      </div>
    </form>
  );
}
