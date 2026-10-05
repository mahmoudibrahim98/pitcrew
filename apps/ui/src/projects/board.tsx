// The board: tasks in columns by status, optionally in one lane per workstream. Dragging a card, or
// picking it up with the keyboard, asks the hub to move it; the card shows in its new column while
// the request is out, and snaps back with the server's message if the hub says no.

import { ToggleGroup } from 'radix-ui';
import {
  useEffect,
  useId,
  useState,
  type FormEvent,
  type KeyboardEvent,
  type PointerEvent,
  type ReactNode,
} from 'react';
import { Button, Dialog, DialogContent } from '../design/index.ts';
import { NewTaskDialog } from './new-task.tsx';
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
import { useOptimisticMoves } from './moves.ts';
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
  /** "My tasks": every task assigned to this member, across projects. */
  assignee?: MemberId;
  /** Initial grouping; people can switch it. */
  groupBy?: BoardGrouping;
  title?: string;
  /** Where a card leads. Without one, the board opens its own task drawer. */
  onOpenTask?: (task: TaskId) => void;
}

export function Board({ project, workstream, assignee, groupBy = 'status', title = 'Board', onOpenTask }: BoardProps) {
  const filters: TaskFilters = {};
  if (project !== undefined) filters.project = project;
  if (workstream !== undefined) filters.workstream = workstream;
  if (assignee !== undefined) filters.assignee = assignee;

  const headingId = useId();
  const nav = useProjectsNav();
  const tasks = useTasks(filters);
  const sessions = useSessions();
  const inbox = useInbox();
  const members = useMemberMap();
  const workstreams = useWorkstreams(project);
  const parent = useOptionalWorkstream(project === undefined ? workstream : undefined);
  const move = useMoveTask();
  const create = useCreateTask();
  const moves = useOptimisticMoves({
    // `mutateAsync`, not `mutate`: per-call callbacks of `mutate` fire only for the latest call, and
    // several cards can be in flight at once.
    send: (task, to) => move.mutateAsync({ task: task.id, to }),
    listKey: keys.tasks.list(filters),
  });

  const [grouping, setGrouping] = useState<BoardGrouping>(groupBy);
  const [drawerTask, setDrawerTask] = useState<TaskId | null>(null);
  const [newTask, setNewTask] = useState<{ project: string; workstream?: string; status: TaskStatus } | null>(null);

  const workstreamNames = new Map((workstreams.data ?? []).map((w) => [w.id, w.name]));
  const list = (tasks.data ?? []).filter((task) => !task.archived);
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

      {[...moves.notices].map(([task, notice]) => (
        <div
          key={task}
          role="alert"
          className="flex items-start gap-2 rounded-sm border border-risk bg-risk-soft p-2 text-sm text-risk"
        >
          <p className="flex-1">{notice.text}</p>
          <Button variant="ghost" onClick={() => moves.dismiss(task)}>
            Dismiss<span className="sr-only"> the note about {notice.key}</span>
          </Button>
        </div>
      ))}
      {tasks.error !== null && <ErrorNote error={tasks.error} what="load the tasks" />}
      {create.error !== null && <ErrorNote error={create.error} what="add the task" />}
      {tasks.isPending && tasks.error === null && <p className="text-sm text-ink-2">Loading tasks…</p>}

      {tasks.data !== undefined && (
        <BoardView
          tasks={list}
          statusOf={moves.statusOf}
          pending={moves.inFlight}
          sessions={sessionsByTask(sessions.data ?? [])}
          needsYou={needsYou}
          members={members}
          lanes={lanes}
          laneOf={(task) => (grouping === 'status' ? 'all' : (task.workstream ?? NO_WORKSTREAM))}
          onMove={moves.move}
          onOpen={open}
          {...(createIn === undefined ? {} : { onCreate: createIn })}
          onNewTask={projectId === undefined ? undefined : (status, lane) => {
            const inWorkstream = lane !== 'all' && lane !== NO_WORKSTREAM ? lane : workstream;
            setNewTask({ project: projectId, status, ...(inWorkstream === undefined ? {} : { workstream: inWorkstream }) });
          }}
        />
      )}

      {drawerTask !== null && (
        <TaskDrawer taskId={drawerTask} open onOpenChange={(isOpen) => !isOpen && setDrawerTask(null)} />
      )}
      {newTask !== null && <Dialog open onOpenChange={(open) => { if (!open) setNewTask(null); }}>
        <DialogContent title="New task"><NewTaskDialog defaults={newTask} close={() => setNewTask(null)} /></DialogContent>
      </Dialog>}
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
  onNewTask?: ((status: TaskStatus, lane: string) => void) | undefined;
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
  onNewTask,
}: BoardViewProps) {
  const instructionsId = useId();
  const [drag, setDrag] = useState<Drag | null>(null);
  const [over, setOver] = useState<string | null>(null);
  const [picked, setPicked] = useState<{ task: TaskId; target: TaskStatus } | null>(null);
  // A card moved with the keyboard keeps focus: wherever it shows next (its new column, or back
  // where it was if the hub says no), it takes focus once per column.
  const [keepFocus, setKeepFocus] = useState<{ task: TaskId; tookIn?: TaskStatus } | null>(null);
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

  // The card that should take focus when it next renders, if any. A virtualised column scrolls to
  // it (`reveal`), since it may be far below what is rendered.
  const focusCard = keepFocus === null ? undefined : find(keepFocus.task);
  const pendingFocus =
    focusCard !== undefined && keepFocus?.tookIn !== statusOf(focusCard) ? focusCard.id : undefined;

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
    setKeepFocus(null);
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
      setKeepFocus(null);
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
    setKeepFocus({ task: task.id });
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

  const laneHeading = lanes.length > 1 || lanes[0]?.title !== undefined;
  const pickedTask = picked === null ? undefined : find(picked.task);

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
        headingLevel={laneHeading ? 5 : 4}
        takeFocus={pendingFocus === task.id}
        onFocusTaken={() => setKeepFocus({ task: task.id, tookIn: statusOf(task) })}
        onOpen={() => onOpen(task)}
        onHandleClick={handleClick(task)}
        onHandleKeyDown={handleKeyDown(task)}
        onHandleBlur={handleBlur(task)}
        onPointerDown={startDrag(task, lane)}
      />
    );
  };

  return (
    <div
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
                  onNewTask={onNewTask === undefined ? undefined : () => onNewTask(status, lane.id)}
                  onSubmitAdd={
                    onCreate === undefined
                      ? undefined
                      : (title) => onCreate(status, lane.id, title, () => setAdding(null))
                  }
                >
                  <CardList
                    items={cards}
                    itemKey={(t) => t.id}
                    renderItem={renderCard(lane.id)}
                    {...(pendingFocus === undefined ? {} : { reveal: pendingFocus })}
                  />
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
  onNewTask,
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
  onNewTask: (() => void) | undefined;
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
      {onNewTask !== undefined && <Button variant="ghost" onClick={onNewTask}>New task in {label}</Button>}
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
