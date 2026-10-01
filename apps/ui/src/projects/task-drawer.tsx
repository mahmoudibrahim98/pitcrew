// The task drawer: fields, subtasks (the agent's plan lines marked and read-only), the agent run,
// dependencies, comments with @mentions, and the task's history from activity.

import { Dialog } from 'radix-ui';
import { useId, useState, type ComponentType, type FormEvent, type ReactNode } from 'react';
import { Button, StatusPill } from '../design/index.ts';
import type { Member, MemberId, Session, Subtask, Task, TaskId, TaskStatus } from '../data/index.ts';
import { cx } from '../lib/cx.ts';
import { AuthorAvatar, EventList } from './activity.tsx';
import {
  mentionsIn,
  newUlid,
  useActivity,
  useAssignTask,
  useDispatchTask,
  useMe,
  useMemberMap,
  useMembers,
  useMoveTask,
  useNames,
  usePostComment,
  useReplaceSubtasks,
  useSessions,
  useTask,
  useTaskMap,
} from './data.ts';
import { PRIORITY, SESSION_STATE, STATUS_ORDER, TASK_STATUS, formatDay, formatWhen, isWebUrl } from './format.ts';
import { useProjectsNav } from './nav.tsx';
import { MemberChip, memberLabel } from './people.tsx';
import { ErrorNote, Field, MaybeLink, inputClass } from './ui.tsx';

type TitleComponent = ComponentType<{ className?: string; children?: ReactNode }> | 'h2';

export function TaskDrawer({
  taskId,
  open,
  onOpenChange,
}: {
  taskId: TaskId;
  open: boolean;
  onOpenChange: (open: boolean) => void;
}) {
  return (
    <Dialog.Root open={open} onOpenChange={onOpenChange}>
      <Dialog.Portal>
        <Dialog.Overlay className="fixed inset-0 z-40 bg-ink/20" />
        <Dialog.Content
          aria-describedby={undefined}
          className="fixed inset-y-0 right-0 z-50 flex w-full max-w-2xl flex-col overflow-y-auto border-l border-line bg-bg p-5 shadow-pop"
        >
          <TaskDetail
            taskId={taskId}
            Title={Dialog.Title}
            close={
              <Dialog.Close asChild>
                <Button variant="ghost" aria-label="Close">
                  <span aria-hidden>✕</span>
                </Button>
              </Dialog.Close>
            }
          />
        </Dialog.Content>
      </Dialog.Portal>
    </Dialog.Root>
  );
}

/** The drawer's content, usable on a page of its own. */
export function TaskDetail({
  taskId,
  Title = 'h2',
  close,
  combineHeading = false,
}: {
  taskId: TaskId;
  Title?: TitleComponent;
  close?: ReactNode;
  /** One heading reading "KEY · Title" instead of the key on its own line above it (a page of its own, not a drawer). */
  combineHeading?: boolean;
}) {
  const task = useTask(taskId);
  const me = useMe();
  const person = me.data?.kind === 'human';

  if (task.data === undefined) {
    return (
      <div className="flex flex-col gap-3">
        <header className="flex items-center gap-2">
          <Title className="text-xl font-semibold">Task</Title>
          <span className="ml-auto">{close}</span>
        </header>
        {task.error !== null ? (
          <ErrorNote error={task.error} what="load the task" />
        ) : (
          <p className="text-sm text-ink-2">Loading…</p>
        )}
      </div>
    );
  }
  const data = task.data;
  // A dialog's title reads as roughly an h2 (Radix's Dialog.Title), so its sections are h3; a
  // page's own `<h1>` (combineHeading) needs h2 sections instead, to keep the order unbroken.
  const level: SectionLevel = combineHeading ? 2 : 3;
  return (
    <article className="flex flex-col gap-5">
      <header className="flex items-start gap-2">
        {combineHeading ? (
          <Title className="text-xl leading-tight font-semibold">{`${data.key} · ${data.title}`}</Title>
        ) : (
          <div className="flex flex-col gap-1">
            <span className="font-mono text-xs text-ink-2">{data.key}</span>
            <Title className="text-xl leading-tight font-semibold">{data.title}</Title>
          </div>
        )}
        <span className="ml-auto">{close}</span>
      </header>
      <Fields task={data} person={person} />
      {data.description !== '' && <p className="text-md leading-relaxed whitespace-pre-wrap">{data.description}</p>}
      <Subtasks task={data} person={person} level={level} />
      <AgentRun task={data} person={person} level={level} />
      <Dependencies task={data} level={level} />
      <Comments task={data} level={level} />
      <History task={data} level={level} />
    </article>
  );
}

/** A dialog's title (`Dialog.Title`, ~h2) is followed by h3 sections; a page's own `<h1>` needs h2
 * sections instead, so nothing skips a level. */
type SectionLevel = 2 | 3;

function Section({
  title,
  level = 3,
  children,
  className,
}: {
  title: string;
  level?: SectionLevel;
  children: ReactNode;
  className?: string;
}) {
  const id = useId();
  const Heading = level === 2 ? 'h2' : 'h3';
  return (
    <section aria-labelledby={id} className={cx('flex flex-col gap-2', className)}>
      <Heading id={id} className="text-md font-semibold">
        {title}
      </Heading>
      {children}
    </section>
  );
}

function Fields({ task, person }: { task: Task; person: boolean }) {
  const members = useMembers();
  const names = useNames();
  const nav = useProjectsNav();
  const move = useMoveTask();
  const assign = useAssignTask();
  const memberMap = new Map((members.data ?? []).map((m) => [m.id, m]));
  const status: TaskStatus = move.isPending ? move.variables.to : task.status;
  const assignee = assign.isPending ? (assign.variables.assignee ?? undefined) : task.assignee;
  const openWorkstream = nav.openWorkstream;
  const openProject = nav.openProject;
  const workstream = task.workstream;
  return (
    <div className="flex flex-col gap-2">
      <dl className="grid grid-cols-[8rem_1fr] items-center gap-x-3 gap-y-2 text-sm">
        <dt className="text-ink-2">Status</dt>
        <dd>
          {person ? (
            <select
              aria-label="Status"
              value={status}
              disabled={move.isPending}
              onChange={(e) => move.mutate({ task: task.id, to: e.target.value as TaskStatus })}
              className={cx(inputClass, 'w-auto')}
            >
              {STATUS_ORDER.map((s) => (
                <option key={s} value={s}>
                  {TASK_STATUS[s].label}
                </option>
              ))}
            </select>
          ) : (
            <StatusPill tone={TASK_STATUS[status].tone}>{TASK_STATUS[status].label}</StatusPill>
          )}
        </dd>
        <dt className="text-ink-2">Priority</dt>
        <dd>{PRIORITY[task.priority].label}</dd>
        <dt className="text-ink-2">Assignee</dt>
        <dd>
          {person ? (
            <select
              aria-label="Assignee"
              value={assignee ?? ''}
              disabled={assign.isPending}
              onChange={(e) => assign.mutate({ task: task.id, assignee: e.target.value === '' ? null : e.target.value })}
              className={cx(inputClass, 'w-auto')}
            >
              <option value="">Unassigned</option>
              {(members.data ?? []).map((m) => (
                <option key={m.id} value={m.id}>
                  {m.handle} · {memberLabel(m, m.owner === undefined ? undefined : memberMap.get(m.owner))}
                </option>
              ))}
            </select>
          ) : assignee !== undefined && memberMap.get(assignee) !== undefined ? (
            <MemberChip member={memberMap.get(assignee) as Member} />
          ) : (
            'Unassigned'
          )}
        </dd>
        <dt className="text-ink-2">Due</dt>
        <dd>{task.due === undefined ? 'No due date' : formatDay(task.due)}</dd>
        {task.labels.length > 0 && (
          <>
            <dt className="text-ink-2">Labels</dt>
            <dd className="flex flex-wrap gap-1">
              {task.labels.map((label) => (
                <span key={label} className="rounded-pill bg-sunken px-2 text-xs text-ink-2">
                  {label}
                </span>
              ))}
            </dd>
          </>
        )}
        <dt className="text-ink-2">Project</dt>
        <dd>
          <MaybeLink onOpen={openProject === undefined ? undefined : () => openProject(task.project)}>
            {names.project(task.project)}
          </MaybeLink>
        </dd>
        {workstream !== undefined && (
          <>
            <dt className="text-ink-2">Workstream</dt>
            <dd>
              <MaybeLink onOpen={openWorkstream === undefined ? undefined : () => openWorkstream(workstream)}>
                {names.workstream(workstream)}
              </MaybeLink>
            </dd>
          </>
        )}
        {task.source !== undefined && (
          <>
            <dt className="text-ink-2">Source</dt>
            <dd>
              {task.source.url !== undefined && isWebUrl(task.source.url) ? (
                <a href={task.source.url} target="_blank" rel="noopener noreferrer" className="text-accent-text underline">
                  {task.source.system}: {task.source.key}
                </a>
              ) : (
                `${task.source.system}: ${task.source.key}`
              )}
            </dd>
          </>
        )}
      </dl>
      {move.error !== null && <ErrorNote error={move.error} what="move the task" />}
      {assign.error !== null && <ErrorNote error={assign.error} what="assign the task" />}
    </div>
  );
}

function Subtasks({ task, person, level }: { task: Task; person: boolean; level: SectionLevel }) {
  const noteId = useId();
  const members = useMemberMap();
  const replace = useReplaceSubtasks();
  const [text, setText] = useState('');
  const subtasks = replace.isPending ? replace.variables.subtasks : task.subtasks;
  const hasPlan = subtasks.some((s) => s.source.kind === 'agent_plan');

  const save = (next: Subtask[], done?: () => void) =>
    replace.mutate({ task: task.id, subtasks: next }, { onSuccess: () => done?.() });
  const toggle = (id: string) => save(subtasks.map((s) => (s.id === id ? { ...s, done: !s.done } : s)));
  const add = (e: FormEvent) => {
    e.preventDefault();
    if (text.trim() === '') return;
    save([...subtasks, { id: newUlid(), text: text.trim(), done: false, source: { kind: 'human' } }], () => setText(''));
  };

  return (
    <Section title="Subtasks" level={level}>
      {hasPlan && (
        <p id={noteId} className="text-xs text-ink-2">
          Lines marked “Agent plan” mirror the agent’s own plan; only the agent changes them.
        </p>
      )}
      {subtasks.length === 0 ? (
        <p className="text-sm text-ink-2">No subtasks.</p>
      ) : (
        <ul aria-label="Subtasks" className="flex flex-col gap-1">
          {subtasks.map((s) => {
            const plan = s.source.kind === 'agent_plan';
            const agent = s.source.kind === 'agent_plan' ? members.get(s.source.agent) : undefined;
            return (
              <li key={s.id} data-source={s.source.kind} className="flex items-center gap-2 text-sm">
                <label className="flex flex-1 items-center gap-2">
                  <input
                    type="checkbox"
                    checked={s.done}
                    disabled={plan || !person || replace.isPending}
                    aria-describedby={plan ? noteId : undefined}
                    onChange={() => toggle(s.id)}
                  />
                  <span className={cx(s.done && 'text-ink-2 line-through')}>{s.text}</span>
                </label>
                {plan && (
                  <span className="inline-flex items-center gap-1 rounded-sm bg-accent-soft px-1.5 text-xs text-accent-text">
                    Agent plan{agent === undefined ? '' : ` · ${agent.handle}`}
                  </span>
                )}
              </li>
            );
          })}
        </ul>
      )}
      {person && (
        <form onSubmit={add} className="flex gap-2">
          <input
            aria-label="New subtask"
            placeholder="Add a subtask"
            value={text}
            onChange={(e) => setText(e.target.value)}
            className={inputClass}
          />
          <Button type="submit" disabled={replace.isPending}>
            Add
          </Button>
        </form>
      )}
      {replace.error !== null && <ErrorNote error={replace.error} what="save the subtasks" />}
    </Section>
  );
}

function AgentRun({ task, person, level }: { task: Task; person: boolean; level: SectionLevel }) {
  const sessions = useSessions({ task: task.id });
  const members = useMemberMap();
  const all = [...(sessions.data ?? [])].sort(
    (a, b) => SESSION_STATE[a.state].rank - SESSION_STATE[b.state].rank || b.last_activity - a.last_activity,
  );
  const running = all.filter((s) => s.state !== 'ended');
  const ended = all.filter((s) => s.state === 'ended');
  return (
    <Section title="Agent run" level={level} className="rounded-md border border-line bg-card p-3">
      {sessions.error !== null && <ErrorNote error={sessions.error} what="load the sessions" />}
      {running.length === 0 && sessions.data !== undefined && (
        <p className="text-sm text-ink-2">No agent is working on this task.</p>
      )}
      {running.map((session) => (
        <RunRow key={session.id} session={session} members={members} />
      ))}
      {ended.length > 0 && (
        <p className="text-xs text-ink-2">
          {ended.length === 1 ? 'One earlier run' : `${ended.length} earlier runs`}, last active{' '}
          {formatWhen(ended.reduce((at, s) => Math.max(at, s.last_activity), 0))}.
        </p>
      )}
      {person && <DispatchForm task={task} />}
    </Section>
  );
}

function RunRow({ session, members }: { session: Session; members: ReadonlyMap<MemberId, Member> }) {
  const nav = useProjectsNav();
  const names = useNames();
  const agent = session.agent === undefined ? undefined : members.get(session.agent);
  const owner = agent?.owner === undefined ? undefined : members.get(agent.owner);
  const openSession = nav.openSession;
  const state = SESSION_STATE[session.state];
  const soon = 'Opens once the agent console is connected';
  return (
    <div className="flex flex-col gap-1.5 rounded-sm bg-sunken p-2" data-session={session.id}>
      <div className="flex flex-wrap items-center gap-2 text-sm">
        {agent !== undefined && <MemberChip member={agent} owner={owner} />}
        <StatusPill tone={state.tone}>{state.label}</StatusPill>
        <span className="text-xs text-ink-2">
          {session.engine} · {names.machine(session.machine)}
          {session.branch === undefined ? '' : ` · ${session.branch}`}
        </span>
      </div>
      {session.status_line !== undefined && <p className="text-sm">{session.status_line}</p>}
      <div className="flex flex-wrap items-center gap-2">
        <Button
          disabled={openSession === undefined}
          title={openSession === undefined ? soon : undefined}
          onClick={() => openSession?.(session.id, 'chat')}
        >
          Open chat
        </Button>
        <Button
          disabled={openSession === undefined}
          title={openSession === undefined ? soon : undefined}
          onClick={() => openSession?.(session.id, 'terminal')}
        >
          Open terminal
        </Button>
        <span className="text-xs text-ink-2">Last activity {formatWhen(session.last_activity)}</span>
      </div>
    </div>
  );
}

function DispatchForm({ task }: { task: Task }) {
  const members = useMembers();
  const dispatch = useDispatchTask();
  const agents = (members.data ?? []).filter((m) => m.kind === 'agent');
  const [agent, setAgent] = useState('');
  const [brief, setBrief] = useState('');
  const chosen = agent !== '' ? agent : (agents[0]?.id ?? '');
  const submit = (e: FormEvent) => {
    e.preventDefault();
    if (chosen === '') return;
    const text = brief.trim();
    dispatch.mutate(text === '' ? { task: task.id, agent: chosen } : { task: task.id, agent: chosen, brief: text }, {
      onSuccess: () => setBrief(''),
    });
  };
  return (
    <form onSubmit={submit} className="flex flex-col gap-2 border-t border-line pt-2">
      <div className="flex flex-wrap items-end gap-2">
        <Field label="Dispatch to">
          {(id) => (
            <select id={id} value={chosen} onChange={(e) => setAgent(e.target.value)} className={cx(inputClass, 'w-auto')}>
              {agents.map((a) => (
                <option key={a.id} value={a.id}>
                  {a.handle}
                </option>
              ))}
            </select>
          )}
        </Field>
        <Button type="submit" variant="primary" disabled={dispatch.isPending || chosen === ''}>
          Dispatch
        </Button>
      </div>
      <Field label="Brief (optional)" hint="Defaults to the task's description.">
        {(id) => (
          <textarea id={id} rows={2} value={brief} onChange={(e) => setBrief(e.target.value)} className={inputClass} />
        )}
      </Field>
      {dispatch.isSuccess && (
        <p role="status" className="text-sm text-ok">
          Dispatched. The session appears above when it starts.
        </p>
      )}
      {dispatch.error !== null && <ErrorNote error={dispatch.error} what="dispatch" />}
    </form>
  );
}

function Dependencies({ task, level }: { task: Task; level: SectionLevel }) {
  const tasks = useTaskMap();
  const nav = useProjectsNav();
  const blockedBy = task.blocked_by.map((id) => tasks.get(id) ?? id);
  const blocks = [...tasks.values()].filter((t) => t.blocked_by.includes(task.id));
  // One below the section's own heading, so nothing skips a level in either mode.
  const SubHeading = level === 2 ? 'h3' : 'h4';
  if (blockedBy.length === 0 && blocks.length === 0) {
    return (
      <Section title="Dependencies" level={level}>
        <p className="text-sm text-ink-2">No dependencies.</p>
      </Section>
    );
  }
  const openTask = nav.openTask;
  const row = (t: Task | string) =>
    typeof t === 'string' ? (
      <li key={t} className="font-mono text-xs text-ink-2">
        {t}
      </li>
    ) : (
      <li key={t.id} className="flex items-center gap-2 text-sm">
        <span className="font-mono text-xs text-ink-2">{t.key}</span>
        <MaybeLink onOpen={openTask === undefined ? undefined : () => openTask(t.id)}>{t.title}</MaybeLink>
        <StatusPill tone={TASK_STATUS[t.status].tone} className="ml-auto">
          {TASK_STATUS[t.status].label}
        </StatusPill>
      </li>
    );
  return (
    <Section title="Dependencies" level={level}>
      {blockedBy.length > 0 && (
        <>
          <SubHeading className="text-xs font-medium text-ink-2">Blocked by</SubHeading>
          <ul aria-label="Blocked by" className="flex flex-col gap-1">
            {blockedBy.map(row)}
          </ul>
        </>
      )}
      {blocks.length > 0 && (
        <>
          <SubHeading className="text-xs font-medium text-ink-2">Blocks</SubHeading>
          <ul aria-label="Blocks" className="flex flex-col gap-1">
            {blocks.map(row)}
          </ul>
        </>
      )}
    </Section>
  );
}

/** Splits text into plain runs and known `@handles`. */
export function withMentions(text: string, members: readonly Member[]): { text: string; member?: Member }[] {
  const byHandle = new Map(members.map((m) => [m.handle.toLowerCase(), m]));
  return text
    .split(/(@[\w.-]*\w)/)
    .filter((part) => part !== '')
    .map((part) => {
      const member = byHandle.get(part.toLowerCase());
      return member === undefined ? { text: part } : { text: part, member };
    });
}

function CommentText({ text, members }: { text: string; members: readonly Member[] }) {
  return (
    <p className="text-sm whitespace-pre-wrap">
      {withMentions(text, members).map((part, i) =>
        part.member === undefined ? (
          part.text
        ) : (
          // Parts never reorder, so the index is a stable key.
          <span key={i} className="font-medium text-accent-text" title={part.member.name}>
            {part.text}
          </span>
        ),
      )}
    </p>
  );
}

function Comments({ task, level }: { task: Task; level: SectionLevel }) {
  const activity = useActivity({ task: task.id });
  const members = useMembers();
  const memberMap = useMemberMap();
  const post = usePostComment();
  const [text, setText] = useState('');
  const list = members.data ?? [];
  const comments = (activity.data?.events ?? []).filter((e) => e.body.type === 'comment_posted').reverse();
  const partial = /(?:^|\s)(@[\w.-]*)$/.exec(text)?.[1];
  const suggestions =
    partial === undefined
      ? []
      : list.filter((m) => m.handle.toLowerCase().startsWith(partial.toLowerCase()) && m.handle !== partial).slice(0, 5);

  const submit = (e: FormEvent) => {
    e.preventDefault();
    const body = text.trim();
    if (body === '') return;
    post.mutate({ task: task.id, text: body, mentions: mentionsIn(body, list) }, { onSuccess: () => setText('') });
  };

  return (
    <Section title="Comments" level={level}>
      {comments.length === 0 ? (
        <p className="text-sm text-ink-2">No comments yet.</p>
      ) : (
        <ul aria-label="Comments" className="flex flex-col gap-3">
          {comments.map((event) =>
            event.body.type === 'comment_posted' ? (
              <li key={event.id} className="flex gap-2">
                <AuthorAvatar event={event} members={memberMap} />
                <div className="flex min-w-0 flex-col gap-0.5">
                  <p className="text-xs text-ink-2">
                    <span className="font-medium text-ink">{memberMap.get(event.author)?.handle ?? 'Someone'}</span> ·{' '}
                    {formatWhen(event.at)}
                  </p>
                  <CommentText text={event.body.data.text} members={list} />
                </div>
              </li>
            ) : null,
          )}
        </ul>
      )}
      <form onSubmit={submit} className="flex flex-col gap-2">
        <Field label="Add a comment" hint="Mention someone with @ and their handle.">
          {(id) => (
            <textarea id={id} rows={2} value={text} onChange={(e) => setText(e.target.value)} className={inputClass} />
          )}
        </Field>
        {suggestions.length > 0 && (
          <div className="flex flex-wrap gap-1" role="group" aria-label="Mention suggestions">
            {suggestions.map((m) => (
              <Button
                key={m.id}
                variant="ghost"
                onClick={() => setText(`${text.slice(0, text.length - (partial?.length ?? 0))}${m.handle} `)}
              >
                {m.handle}
              </Button>
            ))}
          </div>
        )}
        <div>
          <Button type="submit" variant="primary" disabled={post.isPending || text.trim() === ''}>
            Comment
          </Button>
        </div>
        {post.error !== null && <ErrorNote error={post.error} what="post the comment" />}
      </form>
    </Section>
  );
}

function History({ task, level }: { task: Task; level: SectionLevel }) {
  const activity = useActivity({ task: task.id });
  const members = useMemberMap();
  const names = useNames();
  const events = (activity.data?.events ?? []).filter((e) => e.body.type !== 'comment_posted').reverse();
  return (
    <Section title="History" level={level}>
      {activity.error !== null && <ErrorNote error={activity.error} what="load the history" />}
      {events.length === 0 && activity.data !== undefined && <p className="text-sm text-ink-2">Nothing yet.</p>}
      {events.length > 0 && <EventList events={events} members={members} names={names} label="History" />}
      {activity.data !== undefined && !activity.data.at_start && (
        <div>
          <Button variant="ghost" onClick={activity.loadOlder} disabled={activity.isFetching}>
            Load older
          </Button>
        </div>
      )}
    </Section>
  );
}
