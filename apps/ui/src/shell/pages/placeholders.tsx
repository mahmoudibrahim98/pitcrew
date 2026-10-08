// Placeholder pages at the shell's well-known paths, until the Projects (N) and Agent console (M)
// features register their own. They show just enough to navigate by.

import { Link, useParams } from '@tanstack/react-router';
import { useMemo, type ReactNode } from 'react';
import {
  useApi,
  useMachines,
  useMe,
  useOrchestrator,
  useProjects,
  useSessions,
  useTasks,
  useWorkstreams,
  type Engine,
  type Health,
  type SessionState,
  type TaskStatus,
} from '../../data/index.ts';
import { StatusPill, type Tone } from '../../design/index.ts';
import { createHubOnboardingApi } from '../../onboarding/hub-api.ts';
import { SignInPanel } from '../../onboarding/sign-in-panel.tsx';
import { isOpenTask, useMyOpenAsks } from '../data.ts';
import { useWorkspaceId } from '../layout.ts';
import { paths } from '../paths.ts';
import { NotFoundPage } from './not-found.tsx';
import { LOADING, Page } from './page.tsx';

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

const SESSION: Record<SessionState, Tone> = {
  starting: 'accent',
  working: 'progress',
  waiting: 'warn',
  idle: 'neutral',
  ended: 'neutral',
  unreachable: 'risk',
};

const LINK = 'font-medium text-ink hover:underline underline-offset-2';

function useRouteParams(): { project?: string; workstream?: string; task?: string; session?: string } {
  return useParams({ strict: false });
}

function List({ label, empty, children }: { label: string; empty: string; children: ReactNode[] }) {
  return (
    <section className="rounded-lg border border-line bg-card">
      <h2 className="border-b border-line px-4 py-2 text-sm font-semibold">{label}</h2>
      {children.length === 0 ? (
        <p className="px-4 py-3 text-sm text-ink-2">{empty}</p>
      ) : (
        <ul className="divide-y divide-line">{children}</ul>
      )}
    </section>
  );
}

function Row({ children }: { children: ReactNode }) {
  return <li className="flex items-center gap-3 px-4 py-2 text-sm">{children}</li>;
}

export function HomePage() {
  const ws = useWorkspaceId();
  const projects = useProjects().data ?? [];
  return (
    <Page title="Home">
      <List label="Projects" empty="No projects yet.">
        {projects.map((p) => (
          <Row key={p.id}>
            <span className="w-10 font-mono text-xs text-ink-2">{p.key}</span>
            <Link to={paths.project(ws, p.id)} className={LINK}>
              {p.name}
            </Link>
          </Row>
        ))}
      </List>
    </Page>
  );
}

export function InboxPage() {
  const asks = useMyOpenAsks().data ?? [];
  return (
    <Page title="Inbox">
      <List label="Open asks" empty="Nothing needs you.">
        {asks.map((a) => (
          <Row key={a.id}>
            <StatusPill tone="accent">{a.kind}</StatusPill>
            <span className="truncate">{a.title}</span>
          </Row>
        ))}
      </List>
    </Page>
  );
}

export function MyTasksPage() {
  const ws = useWorkspaceId();
  const me = useMe().data?.id;
  const tasks = (useTasks().data ?? []).filter((t) => t.assignee === me && isOpenTask(t));
  return (
    <Page title="My tasks">
      <List label="Open tasks" empty="No open tasks assigned to you.">
        {tasks.map((t) => (
          <Row key={t.id}>
            <span className="w-14 font-mono text-xs text-ink-2">{t.key}</span>
            <Link to={paths.task(ws, t.key)} className={LINK}>
              {t.title}
            </Link>
          </Row>
        ))}
      </List>
    </Page>
  );
}

export function ProjectPage() {
  const ws = useWorkspaceId();
  const { project: id } = useRouteParams();
  const projects = useProjects().data;
  const workstreams = (useWorkstreams().data ?? []).filter((w) => w.project === id);
  const project = projects?.find((p) => p.id === id);
  if (projects !== undefined && project === undefined) return <NotFoundPage />;
  return (
    <Page title={project?.name ?? LOADING} eyebrow={project?.key}>
      <List label="Workstreams" empty="No workstreams yet.">
        {workstreams.map((w) => (
          <Row key={w.id}>
            <Link to={paths.workstream(ws, w.project, w.id)} className={LINK}>
              {w.name}
            </Link>
            <StatusPill tone={HEALTH[w.health].tone}>{HEALTH[w.health].label}</StatusPill>
            <span className="text-xs text-ink-2">{w.status}</span>
          </Row>
        ))}
      </List>
    </Page>
  );
}

export function WorkstreamPage() {
  const ws = useWorkspaceId();
  const { workstream: id } = useRouteParams();
  const workstreams = useWorkstreams().data;
  const workstream = workstreams?.find((w) => w.id === id);
  const tasks = (useTasks().data ?? []).filter((t) => t.workstream === id);
  const project = useProjects().data?.find((p) => p.id === workstream?.project);
  if (workstreams !== undefined && workstream === undefined) return <NotFoundPage />;
  return (
    <Page title={workstream?.name ?? LOADING} eyebrow={project?.name}>
      <List label="Tasks" empty="No tasks yet.">
        {tasks.map((t) => (
          <Row key={t.id}>
            <span className="w-14 font-mono text-xs text-ink-2">{t.key}</span>
            <Link to={paths.task(ws, t.key)} className={LINK}>
              {t.title}
            </Link>
            <span className="ml-auto">
              <StatusPill tone={TASK[t.status].tone}>{TASK[t.status].label}</StatusPill>
            </span>
          </Row>
        ))}
      </List>
    </Page>
  );
}

export function TaskPage() {
  const ws = useWorkspaceId();
  const { task: ref } = useRouteParams();
  const tasks = useTasks().data;
  const task = tasks?.find((t) => t.key === ref || t.id === ref);
  const workstream = useWorkstreams().data?.find((w) => w.id === task?.workstream);
  if (tasks !== undefined && task === undefined) return <NotFoundPage />;
  return (
    <Page title={task === undefined ? LOADING : `${task.key} · ${task.title}`}>
      {task !== undefined && (
        <dl className="grid grid-cols-[max-content_1fr] gap-x-6 gap-y-2 text-sm">
          <dt className="text-ink-2">Status</dt>
          <dd>
            <StatusPill tone={TASK[task.status].tone}>{TASK[task.status].label}</StatusPill>
          </dd>
          <dt className="text-ink-2">Workstream</dt>
          <dd>
            {workstream === undefined ? (
              'None'
            ) : (
              <Link to={paths.workstream(ws, workstream.project, workstream.id)} className={LINK}>
                {workstream.name}
              </Link>
            )}
          </dd>
        </dl>
      )}
    </Page>
  );
}

export function ConsolePage() {
  const ws = useWorkspaceId();
  const sessions = useSessions().data ?? [];
  return (
    <Page title="Agent console">
      <List label="Sessions" empty="No sessions yet.">
        {sessions.map((s) => (
          <Row key={s.id}>
            <StatusPill tone={SESSION[s.state]}>{s.state}</StatusPill>
            <span className="font-mono text-xs text-ink-2">{s.engine}</span>
            <Link to={paths.session(ws, s.id)} className={`${LINK} truncate`}>
              {s.title ?? s.cwd}
            </Link>
          </Row>
        ))}
      </List>
    </Page>
  );
}

/** `paths.setup`, until the onboarding feature serves its first-run wizard there. */
export function SetupPage() {
  return (
    <Page title="Set up">
      <p className="text-sm text-ink-2">This workspace needs setting up, and this build has no setup wizard.</p>
    </Page>
  );
}

/**
 * The agent CLIs the Orchestrator may answer with, and how each signs in, in its own terminal. The
 * page lists those the hub offers (`GET /v1/orchestrator`'s `engines`: on Windows, Claude Code
 * only); Claude Code alone until it has said.
 */
const SIGN_IN: { id: Engine; engine: string; install: string; signIn: string }[] = [
  { id: 'claude', engine: 'Claude Code', install: 'claude', signIn: 'claude, then /login' },
  { id: 'opencode', engine: 'OpenCode', install: 'opencode', signIn: 'opencode auth login' },
];

/**
 * `paths.signIn`: the agent CLIs on this hub's machine, whether each is signed in, and its own login
 * in a terminal (onboarding's sign-in panel, over `GET /v1/machines/{id}/agents`; the hub's owner
 * only, so anyone else sees why not). How to do it by hand follows.
 */
export function SignInPage() {
  const data = useApi();
  const api = useMemo(() => createHubOnboardingApi({ transport: data.transport }), [data.transport]);
  const own = useMachines()
    .data?.find((m) => m.kind === 'local')
    ?.name.trim();
  const offered = useOrchestrator().data?.engines.map((e) => e.engine);
  const rows = SIGN_IN.filter((row) => (offered === undefined ? row.id === 'claude' : offered.includes(row.id)));
  return (
    <Page title="Sign in to your agents">
      <p className="text-sm text-ink-2">
        The Orchestrator answers with an agent CLI you already use, on this hub&apos;s machine, signed in as
        you.
      </p>
      <SignInPanel api={api} target={{ kind: 'local' }} machineLabel={own === '' ? undefined : own} />
      <p className="text-sm text-ink-2">Or install one there and sign in once, in a terminal:</p>
      <List label="Agent CLIs" empty="">
        {rows.map((row) => (
          <Row key={row.engine}>
            <span className="w-28 font-medium">{row.engine}</span>
            <span className="text-ink-2">
              On the PATH as <code className="font-mono text-xs">{row.install}</code>; sign in with{' '}
              <code className="font-mono text-xs">{row.signIn}</code>
            </span>
          </Row>
        ))}
      </List>
    </Page>
  );
}

export function SessionPage() {
  const { session: id } = useRouteParams();
  const sessions = useSessions().data;
  const session = sessions?.find((s) => s.id === id);
  if (sessions !== undefined && session === undefined) return <NotFoundPage />;
  return (
    <Page title={session?.title ?? session?.cwd ?? LOADING} eyebrow={session?.engine}>
      {session !== undefined && (
        <p className="flex items-center gap-2 text-sm">
          <StatusPill tone={SESSION[session.state]}>{session.state}</StatusPill>
          <span className="text-ink-2">{session.status_line}</span>
        </p>
      )}
    </Page>
  );
}
