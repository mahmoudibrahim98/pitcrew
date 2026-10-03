// A minimal page proving the data layer: projects, workstreams, open asks and live sessions from
// the hub, and a move that comes back through the stream. Kept as a dev-only route (/dev/proof).

import { WorkspaceName } from './workspace-name.tsx';
import { Button, StatusPill, ThemeToggle, type Tone } from '../design/index.ts';
import {
  ApiError,
  useAsks,
  useConnection,
  useGatewayWorkspaces,
  useGatewayWorkspace,
  useMoveTask,
  useProjects,
  useSessions,
  useTasks,
  useWorkspace,
  useWorkstreams,
  type Ask,
  type Health,
  type Project,
  type Session,
  type SessionState,
  type StreamStatus,
  type Task,
  type TaskStatus,
  type Workstream,
} from '../data/index.ts';

const HEALTH: Record<Health, { tone: Tone; label: string }> = {
  on_track: { tone: 'ok', label: 'On track' },
  at_risk: { tone: 'warn', label: 'At risk' },
  blocked: { tone: 'risk', label: 'Blocked' },
};

const TASK: Record<TaskStatus, { tone: Tone; label: string }> = {
  backlog: { tone: 'neutral', label: 'Backlog' },
  todo: { tone: 'neutral', label: 'Todo' },
  in_progress: { tone: 'progress', label: 'In progress' },
  review: { tone: 'accent', label: 'Review' },
  done: { tone: 'ok', label: 'Done' },
  canceled: { tone: 'neutral', label: 'Canceled' },
};

/** Where the proof page's button sends a task next. */
const NEXT: Record<TaskStatus, TaskStatus> = {
  backlog: 'todo',
  todo: 'in_progress',
  in_progress: 'review',
  review: 'done',
  done: 'todo',
  canceled: 'todo',
};

const SESSION: Record<SessionState, Tone> = {
  starting: 'accent',
  working: 'progress',
  waiting: 'warn',
  idle: 'neutral',
  ended: 'neutral',
  unreachable: 'risk',
};

const STREAM: Record<StreamStatus, { tone: Tone; label: string }> = {
  live: { tone: 'ok', label: 'Live' },
  connecting: { tone: 'accent', label: 'Connecting' },
  reconnecting: { tone: 'warn', label: 'Reconnecting' },
  stopped: { tone: 'neutral', label: 'Offline' },
};

export function ProofPage() {
  // It reads the browser's one hub; the desktop app has a data scope per workspace instead.
  return useGatewayWorkspaces() === null ? (
    <Proof />
  ) : (
    <main className="mx-auto min-h-dvh max-w-5xl bg-bg px-6 py-8 text-sm text-ink">The proof page runs in a browser.</main>
  );
}

function Proof() {
  const workspace = useWorkspace();
  const projects = useProjects();
  const workstreams = useWorkstreams();
  const tasks = useTasks();
  const sessions = useSessions();
  const asks = useAsks({ state: 'open' });
  const { status, problem } = useConnection();
  const gateway = useGatewayWorkspace();

  const error = [workspace, projects, workstreams, tasks, sessions, asks].find((q) => q.error)?.error;

  return (
    <main className="mx-auto min-h-dvh max-w-5xl bg-bg px-6 py-8 text-ink">
      <header className="mb-6 flex items-center gap-3">
        <h1 className="text-xl font-semibold"><WorkspaceName workspace={gateway ?? { name: workspace.data?.workspace.name ?? 'PitCrew' }} /></h1>
        <span data-testid="stream-status">
          {problem === 'unauthorized' ? (
            <StatusPill tone="risk">Token rejected</StatusPill>
          ) : (
            <StatusPill tone={STREAM[status].tone}>{STREAM[status].label}</StatusPill>
          )}
        </span>
        <div className="ml-auto">
          <ThemeToggle />
        </div>
      </header>

      {error !== undefined && error !== null && (
        <p role="alert" className="mb-4 rounded-md bg-risk-soft px-3 py-2 text-sm text-risk">
          {error instanceof ApiError ? `${error.code}: ${error.message}` : String(error)}
        </p>
      )}

      <div className="flex flex-col gap-4">
        {projects.data?.map((project) => (
          <ProjectCard
            key={project.id}
            project={project}
            workstreams={workstreams.data?.filter((w) => w.project === project.id) ?? []}
            tasks={tasks.data ?? []}
            sessions={sessions.data ?? []}
            openAsks={openAsksIn(project, asks.data ?? [], tasks.data ?? [], sessions.data ?? [], workstreams.data ?? [])}
          />
        ))}
      </div>
    </main>
  );
}

function openAsksIn(
  project: Project,
  asks: Ask[],
  tasks: Task[],
  sessions: Session[],
  workstreams: Workstream[],
): number {
  return asks.filter((ask) => {
    const task = tasks.find((t) => t.id === ask.task);
    if (task !== undefined) return task.project === project.id;
    const session = sessions.find((s) => s.id === ask.session);
    return workstreams.find((w) => w.id === session?.workstream)?.project === project.id;
  }).length;
}

function ProjectCard(props: {
  project: Project;
  workstreams: Workstream[];
  tasks: Task[];
  sessions: Session[];
  openAsks: number;
}) {
  const { project } = props;
  return (
    <section
      aria-labelledby={`project-${project.id}`}
      className="rounded-lg border border-line bg-card"
    >
      <header className="flex items-center gap-2 border-b border-line px-4 py-3">
        <span className="font-mono text-xs text-ink-2">{project.key}</span>
        <h2 id={`project-${project.id}`} className="text-lg font-semibold">
          {project.name}
        </h2>
        <span className="ml-auto" data-testid={`asks-${project.key}`}>
          <StatusPill tone={props.openAsks > 0 ? 'warn' : 'neutral'}>
            {props.openAsks} open {props.openAsks === 1 ? 'ask' : 'asks'}
          </StatusPill>
        </span>
      </header>
      <ul className="divide-y divide-line">
        {props.workstreams.map((workstream) => (
          <WorkstreamRow
            key={workstream.id}
            workstream={workstream}
            tasks={props.tasks.filter((t) => t.workstream === workstream.id)}
            sessions={props.sessions.filter(
              (s) => s.workstream === workstream.id && s.state !== 'ended',
            )}
          />
        ))}
        {props.workstreams.length === 0 && (
          <li className="px-4 py-3 text-sm text-ink-2">No workstreams yet.</li>
        )}
      </ul>
    </section>
  );
}

function WorkstreamRow(props: { workstream: Workstream; tasks: Task[]; sessions: Session[] }) {
  const { workstream } = props;
  return (
    <li className="px-4 py-3">
      <div className="flex items-center gap-2">
        <h3 className="font-medium">{workstream.name}</h3>
        <StatusPill tone={HEALTH[workstream.health].tone}>{HEALTH[workstream.health].label}</StatusPill>
        <span className="text-xs text-ink-2">{workstream.status}</span>
      </div>

      {props.sessions.length > 0 && (
        <ul aria-label="Live sessions" className="mt-2 flex flex-col gap-1">
          {props.sessions.map((session) => (
            <li key={session.id} className="flex items-center gap-2 text-sm">
              <StatusPill tone={SESSION[session.state]}>{session.state}</StatusPill>
              <span className="font-mono text-xs text-ink-2">{session.engine}</span>
              <span className="truncate text-ink-2">
                {session.status_line ?? session.title ?? session.cwd}
              </span>
            </li>
          ))}
        </ul>
      )}

      {props.tasks.length > 0 && (
        <ul aria-label="Tasks" className="mt-2 flex flex-col gap-1">
          {props.tasks.map((task) => (
            <TaskRow key={task.id} task={task} />
          ))}
        </ul>
      )}
    </li>
  );
}

function TaskRow({ task }: { task: Task }) {
  const move = useMoveTask();
  const next = NEXT[task.status];
  return (
    <li className="flex items-center gap-2 text-sm" data-testid={`task-${task.key}`}>
      <span className="w-14 shrink-0 font-mono text-xs text-ink-2">{task.key}</span>
      <span className="truncate">{task.title}</span>
      <span className="ml-auto" data-testid={`status-${task.key}`}>
        <StatusPill tone={TASK[task.status].tone}>{TASK[task.status].label}</StatusPill>
      </span>
      <Button
        variant="ghost"
        disabled={move.isPending}
        aria-label={`Move ${task.key} to ${TASK[next].label}`}
        onClick={() => move.mutate({ task: task.id, to: next })}
      >
        → {TASK[next].label}
      </Button>
      {move.error !== null && (
        <span role="alert" className="text-xs text-risk">
          {move.error instanceof ApiError ? move.error.code : 'failed'}
        </span>
      )}
    </li>
  );
}
