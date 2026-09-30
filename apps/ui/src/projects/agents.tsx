// Agents at work: running sessions with their live status lines, for a project, a workstream or
// everything.

import type { ProjectId, Session, WorkstreamId } from '../data/index.ts';
import { Button, StatusPill } from '../design/index.ts';
import { useMemberMap, useNames, useSessions, useTaskMap, useWorkstreams } from './data.ts';
import { SESSION_STATE, formatWhen } from './format.ts';
import { useProjectsNav } from './nav.tsx';
import { Avatar } from './people.tsx';
import { ErrorNote, MaybeLink, Panel } from './ui.tsx';

export function runningFirst(a: Session, b: Session): number {
  return SESSION_STATE[a.state].rank - SESSION_STATE[b.state].rank || b.last_activity - a.last_activity;
}

export function AgentsNow({
  project,
  workstream,
  title = 'Agents now',
}: {
  project?: ProjectId;
  workstream?: WorkstreamId;
  title?: string;
}) {
  const sessions = useSessions(workstream === undefined ? {} : { workstream });
  const workstreams = useWorkstreams(project);
  const tasks = useTaskMap(project === undefined ? {} : { project });
  const members = useMemberMap();
  const names = useNames();
  const nav = useProjectsNav();
  const inProject = new Set((workstreams.data ?? []).map((w) => w.id));
  const belongs = (s: Session) =>
    project === undefined ||
    (s.workstream !== undefined && inProject.has(s.workstream)) ||
    (s.task !== undefined && tasks.has(s.task));
  const running = (sessions.data ?? []).filter((s) => s.state !== 'ended' && belongs(s)).sort(runningFirst);
  const openTask = nav.openTask;
  const openSession = nav.openSession;

  return (
    <Panel title={title}>
      {sessions.error !== null && <ErrorNote error={sessions.error} what="load the sessions" />}
      {sessions.data !== undefined && running.length === 0 && (
        <p className="text-sm text-ink-2">No agent is running here right now.</p>
      )}
      {running.length > 0 && (
        <ul aria-label={title} className="flex flex-col divide-y divide-line">
          {running.map((session) => {
            const agent = session.agent === undefined ? undefined : members.get(session.agent);
            const owner = agent?.owner === undefined ? undefined : members.get(agent.owner);
            const state = SESSION_STATE[session.state];
            const task = session.task;
            return (
              <li key={session.id} data-session={session.id} className="flex items-start gap-2 py-2">
                {agent !== undefined ? (
                  <Avatar member={agent} owner={owner} size="md" />
                ) : (
                  <span aria-hidden className="size-7 shrink-0 rounded-sm bg-sunken" />
                )}
                <div className="flex min-w-0 flex-1 flex-col gap-0.5">
                  <p className="flex flex-wrap items-center gap-2 text-sm">
                    <span className="font-medium">{agent?.handle ?? session.engine}</span>
                    <StatusPill tone={state.tone}>{state.label}</StatusPill>
                    {task !== undefined && (
                      <MaybeLink
                        onOpen={openTask === undefined ? undefined : () => openTask(task)}
                        className="font-mono text-xs text-ink-2"
                      >
                        {names.task(task)}
                      </MaybeLink>
                    )}
                  </p>
                  <p className="truncate text-sm text-ink-2">
                    {session.status_line ?? session.title ?? 'No status yet'}
                  </p>
                  <p className="text-xs text-ink-2">
                    {names.machine(session.machine)} · {formatWhen(session.last_activity)}
                  </p>
                </div>
                {openSession !== undefined && (
                  <Button variant="ghost" onClick={() => openSession(session.id, 'chat')}>
                    Open<span className="sr-only"> {session.title ?? 'session'}</span>
                  </Button>
                )}
              </li>
            );
          })}
        </ul>
      )}
    </Panel>
  );
}
