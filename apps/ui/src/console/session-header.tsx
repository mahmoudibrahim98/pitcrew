// A session's header: title, state, engine, machine, branch and folder, links to its task and
// workstream, and its actions (end now; hand off, fork and review when the caller provides them).

import { DropdownMenu } from 'radix-ui';
import { useState } from 'react';
import { ApiError, useMachines, useMembers, useSession, type Task, type Workstream } from '../data/index.ts';
import { Button, EngineLogo, StatusPill } from '../design/index.ts';
import { cx } from '../lib/cx.ts';
import { useEndSession, useTaskById, useWorkstreamById } from './data.ts';
import { LinkSessionDialog } from './link-session.tsx';
import { ENGINE_LABEL, inputBlocked, LIVENESS, sessionTitle, STATE } from './format.ts';

export interface SessionHeaderProps {
  sessionId: string;
  onOpenTask?: (task: Task) => void;
  onOpenWorkstream?: (workstream: Workstream) => void;
  /**
   * Where the task and workstream links point. With them the links are real links (they open in
   * a new tab, and show their target); a plain click still calls `onOpenTask` or `onOpenWorkstream`.
   */
  taskHref?: (task: Task) => string;
  workstreamHref?: (workstream: Workstream) => string;
  /** Placeholders until their flows exist: the menu shows them disabled without a handler. */
  onHandOff?: () => void;
  onFork?: () => void;
  onReview?: () => void;
  /** Only the title row: for a narrow pane given over to the terminal. */
  compact?: boolean;
  /** The "Linked work" landmark's name; the workbench makes it unique per pane. */
  linksLabel?: string | undefined;
  className?: string;
}

const itemClass =
  'flex cursor-default items-center justify-between gap-6 rounded-sm px-2 py-1.5 text-sm outline-none select-none data-disabled:text-muted data-highlighted:bg-hover';

export function SessionHeader(props: SessionHeaderProps) {
  const session = useSession(props.sessionId);
  const machines = useMachines();
  const members = useMembers();
  const task = useTaskById(session.data?.task);
  const workstream = useWorkstreamById(session.data?.workstream);
  const end = useEndSession(props.sessionId);
  const [linking, setLinking] = useState(false);
  const [confirming, setConfirming] = useState(false);

  if (session.data === undefined) {
    return (
      <header className={cx('border-b border-line px-4 py-3 text-sm text-ink-2', props.className)}>
        {session.error !== null ? 'Could not load the session.' : 'Loading the session…'}
      </header>
    );
  }
  const s = session.data;
  const machine = machines.data?.find((m) => m.id === s.machine);
  const agent = s.agent === undefined ? undefined : members.data?.find((m) => m.id === s.agent);
  const state = STATE[s.state];
  const linkedTask = task.data;
  const linkedWorkstream = workstream.data;
  const { onOpenTask, onOpenWorkstream } = props;
  // The same rule as the composer's: an ended or unreachable session takes no command (409, 503).
  const canEnd = inputBlocked(s, machine) === undefined;

  const finish = (mode: 'graceful' | 'kill') =>
    end.mutate(mode, { onSuccess: () => setConfirming(false) });

  return (
    <header className={cx('flex flex-col gap-1.5 border-b border-line px-4 py-3', props.className)}>
      <div className="flex min-w-0 items-center gap-2">
        <EngineLogo engine={s.engine} />
        <h2 className="min-w-0 truncate text-lg font-semibold">{sessionTitle(s)}</h2>
        <StatusPill tone={state.tone}>{state.label}</StatusPill>
        <DropdownMenu.Root modal={false}>
          <DropdownMenu.Trigger asChild>
            <Button variant="ghost" className="ml-auto" aria-label="Session actions">
              Actions
            </Button>
          </DropdownMenu.Trigger>
          <DropdownMenu.Portal>
            <DropdownMenu.Content
              align="end"
              sideOffset={4}
              className="z-50 min-w-44 rounded-md border border-line bg-card p-1 shadow-pop"
            >
              <DropdownMenu.Item className={itemClass} disabled={!canEnd} onSelect={() => setConfirming(true)}>
                End session…
              </DropdownMenu.Item>
              <DropdownMenu.Item className={itemClass} onSelect={() => setLinking(true)}>Link to…</DropdownMenu.Item>
              <DropdownMenu.Separator className="my-1 h-px bg-line" />
              <PlaceholderItem label="Hand off" onSelect={props.onHandOff} />
              <PlaceholderItem label="Fork" onSelect={props.onFork} />
              <PlaceholderItem label="Review" onSelect={props.onReview} />
            </DropdownMenu.Content>
          </DropdownMenu.Portal>
        </DropdownMenu.Root>
      </div>

      {linking && <LinkSessionDialog session={s} onClose={() => setLinking(false)} />}

      {!props.compact && (
        <dl className="flex flex-wrap items-center gap-x-4 gap-y-1 text-xs text-ink-2">
          <div className="flex gap-1">
            <dt className="text-ink-2">Engine</dt>
            <dd>{ENGINE_LABEL[s.engine]}</dd>
          </div>
          {agent !== undefined && (
            <div className="flex gap-1">
              <dt className="text-ink-2">Agent</dt>
              <dd>{agent.handle}</dd>
            </div>
          )}
          <div className="flex gap-1">
            <dt className="text-ink-2">Machine</dt>
            <dd>
              {machine?.name ?? '…'}
              {machine !== undefined && machine.liveness !== 'live' && (
                <span className="ml-1 text-risk">({LIVENESS[machine.liveness].label.toLowerCase()})</span>
              )}
            </dd>
          </div>
          {s.branch !== undefined && (
            <div className="flex gap-1">
              <dt className="text-ink-2">Branch</dt>
              <dd className="font-mono">{s.branch}</dd>
            </div>
          )}
          <div className="flex min-w-0 gap-1">
            <dt className="text-ink-2">Folder</dt>
            <dd className="truncate font-mono" title={s.cwd}>
              {s.cwd}
            </dd>
          </div>
        </dl>
      )}

      {!props.compact && (s.task !== undefined || s.workstream !== undefined) && (
        <nav aria-label={props.linksLabel ?? 'Linked work'} className="flex flex-wrap items-center gap-x-4 gap-y-1 text-xs">
          {linkedTask !== undefined && (
            <LinkedWork
              label="Task"
              text={`${linkedTask.key} · ${linkedTask.title}`}
              href={props.taskHref?.(linkedTask)}
              onOpen={onOpenTask === undefined ? undefined : () => onOpenTask(linkedTask)}
            />
          )}
          {linkedWorkstream !== undefined && (
            <LinkedWork
              label="Workstream"
              text={linkedWorkstream.name}
              href={props.workstreamHref?.(linkedWorkstream)}
              onOpen={onOpenWorkstream === undefined ? undefined : () => onOpenWorkstream(linkedWorkstream)}
            />
          )}
        </nav>
      )}

      {confirming && (
        <div role="alertdialog" aria-label="End this session?" className="flex flex-wrap items-center gap-2 rounded-md bg-warn-soft px-3 py-2 text-sm">
          <span className="mr-auto">End this session? The agent's CLI is asked to exit.</span>
          <Button onClick={() => finish('graceful')} disabled={end.isPending}>
            End
          </Button>
          <Button onClick={() => finish('kill')} disabled={end.isPending}>
            Kill now
          </Button>
          <Button variant="ghost" onClick={() => setConfirming(false)} disabled={end.isPending}>
            Cancel
          </Button>
          {end.error !== null && (
            <span role="alert" className="w-full text-xs text-risk">
              {end.error instanceof ApiError ? end.error.message : 'Could not end the session.'}
            </span>
          )}
        </div>
      )}
    </header>
  );
}

function PlaceholderItem({ label, onSelect }: { label: string; onSelect: (() => void) | undefined }) {
  if (onSelect === undefined) return null;
  return (
    <DropdownMenu.Item className={itemClass} disabled={onSelect === undefined} onSelect={() => onSelect?.()}>
      {label}
    </DropdownMenu.Item>
  );
}

const LINK = 'truncate text-accent-text underline-offset-2 hover:underline';

function LinkedWork({
  label,
  text,
  href,
  onOpen,
}: {
  label: string;
  text: string;
  href: string | undefined;
  onOpen: (() => void) | undefined;
}) {
  let target = <span className="truncate">{text}</span>;
  if (href !== undefined) {
    target = (
      <a
        href={href}
        className={LINK}
        onClick={(event) => {
          // A modified or middle click opens the link the browser's way (a new tab, a window).
          if (onOpen === undefined || event.button !== 0 || event.metaKey || event.ctrlKey || event.shiftKey || event.altKey) {
            return;
          }
          event.preventDefault();
          onOpen();
        }}
      >
        {text}
      </a>
    );
  } else if (onOpen !== undefined) {
    target = (
      <button type="button" onClick={onOpen} className={LINK}>
        {text}
      </button>
    );
  }
  return (
    <span className="flex min-w-0 items-center gap-1">
      <span className="text-ink-2">{label}</span>
      {target}
    </span>
  );
}
