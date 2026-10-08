// Agents at work: active sessions with their live status lines, then a few recent ones, for a
// project, a workstream or everything. A sub-agent is part of its parent's work, never an agent of
// its own: it is left out (api-v1.md, "Sessions").

import { useState } from 'react';
import { topLevel, type ProjectId, type Session, type WorkstreamId } from '../data/index.ts';
import { Button, StatusPill } from '../design/index.ts';
import { useMemberMap, useNames, useSessions, useTaskMap, useWorkstreams } from './data.ts';
import { SESSION_STATE, formatWhen } from './format.ts';
import { useProjectsNav } from './nav.tsx';
import { Avatar } from './people.tsx';
import { ErrorNote, MaybeLink, Panel } from './ui.tsx';

export function runningFirst(a: Session, b: Session): number {
  return SESSION_STATE[a.state].rank - SESSION_STATE[b.state].rank || b.last_activity - a.last_activity;
}

/**
 * Whether a session is at work now: working, waiting or starting, or idle in a terminal PitCrew
 * keeps open (its person may type next). A session found on disk goes idle once it is quiet and
 * never ends unless its CLI says so (api-v1.md, "Sessions"), so idle alone is not "now".
 */
export function isActive(session: Session): boolean {
  switch (session.state) {
    case 'working':
    case 'waiting':
    case 'starting':
      return true;
    case 'idle':
      return session.terminal !== undefined;
    case 'ended':
    case 'unreachable':
      return false;
  }
}

/** Recent sessions (neither active nor sub-agents) shown after the active ones, until "Show all". */
export const RECENT_SHOWN = 3;

export function AgentsNow({
  project,
  workstream,
  title = 'Agents now',
}: {
  project?: ProjectId;
  workstream?: WorkstreamId;
  title?: string;
}) {
  const [showAll, setShowAll] = useState(false);
  // Every session, so a sub-agent is judged by its parent wherever that is linked (a worktree's
  // sub-agent of a session in the main checkout is still no agent of the worktree's).
  const sessions = useSessions();
  const workstreams = useWorkstreams(project);
  const tasks = useTaskMap(project === undefined ? {} : { project });
  const members = useMemberMap();
  const names = useNames();
  const nav = useProjectsNav();
  const inProject = new Set((workstreams.data ?? []).map((w) => w.id));
  const belongs = (s: Session) =>
    (workstream === undefined || s.workstream === workstream) &&
    (project === undefined ||
      (s.workstream !== undefined && inProject.has(s.workstream)) ||
      (s.task !== undefined && tasks.has(s.task)));
  // Sub-agents are left out of the whole list, then the view's sessions are taken from it.
  const agents = topLevel(sessions.data ?? []).filter(belongs);
  const running = agents.filter(isActive).sort(runningFirst);
  const recent = agents.filter((s) => !isActive(s)).sort((a, b) => b.last_activity - a.last_activity);
  const shown = showAll ? [...running, ...recent] : [...running, ...recent.slice(0, RECENT_SHOWN)];
  const hidden = running.length + recent.length - shown.length;
  const openTask = nav.openTask;
  const openSession = nav.openSession;

  return (
    <Panel
      title={title}
      actions={
        hidden > 0 || showAll ? (
          <Button variant="ghost" onClick={() => setShowAll((v) => !v)}>
            {showAll ? 'Show fewer' : `Show all (${running.length + recent.length})`}
          </Button>
        ) : undefined
      }
    >
      {sessions.error !== null && <ErrorNote error={sessions.error} what="load the sessions" />}
      {sessions.data !== undefined && running.length === 0 && (
        <p className="text-sm text-ink-2">No agent is running here right now.</p>
      )}
      {shown.length > 0 && (
        <ul aria-label={title} className="flex flex-col divide-y divide-line">
          {shown.map((session) => {
            const agent = session.agent === undefined ? undefined : members.get(session.agent);
            const owner = agent?.owner === undefined ? undefined : members.get(agent.owner);
            const state = SESSION_STATE[session.state];
            const task = session.task;
            return (
              <li key={session.id} data-session={session.id} className="flex items-start gap-2 py-2">
                {agent !== undefined ? (
                  <Avatar member={agent} owner={owner} size="md" decorative />
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
