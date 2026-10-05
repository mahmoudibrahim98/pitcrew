// A task's writes upstream, in its drawer: each one pending, sent, failed or not sent, with what
// it sends; Retry for a failed one; and, for a person, "Create an issue" (a task that mirrors
// none) or "Comment upstream" (one that does). Asking only proposes: every write waits for the
// approval in the Inbox (api-v1.md, "Outward writes").

import { useId, useState, type FormEvent, type ReactNode } from 'react';
import { Button, StatusPill } from '../../design/index.ts';
import type { Task, UpstreamWrite } from '../../data/index.ts';
import { WRITE_STATE, formatWhen, isWebUrl, tracker } from '../format.ts';
import { useIntegrations } from '../integrations/api.ts';
import { ErrorNote, inputClass } from '../ui.tsx';
import { WriteDiff, writeHeadline } from './approval-card.tsx';
import { useTaskWrites, useWriteActions } from './api.ts';

function Outcome({ write }: { write: UpstreamWrite }) {
  const result = write.result;
  if (result === undefined) return null;
  if (result.outcome === 'sent') {
    const url = result.created?.url ?? result.url;
    return (
      <p className="text-xs text-ink-2">
        {result.created === undefined ? 'Sent' : `Created ${result.created.key}`}
        {write.finished_at === undefined ? '' : ` ${formatWhen(write.finished_at)}`}
        {url !== undefined && isWebUrl(url) && (
          <>
            {' · '}
            <a href={url} target="_blank" rel="noreferrer noopener" className="underline">
              Open
            </a>
          </>
        )}
      </p>
    );
  }
  return <p className="text-xs text-ink-2">{result.outcome === 'failed' ? result.message : result.reason}</p>;
}

function WriteItem({ write, person }: { write: UpstreamWrite; person: boolean }) {
  const { retry } = useWriteActions();
  const [shown, setShown] = useState(false);
  const detailsId = useId();
  return (
    <li data-write={write.proposal.ask} className="flex flex-col gap-1 rounded-sm border border-line px-2 py-1.5">
      <div className="flex flex-wrap items-center gap-2 text-sm">
        <span className="font-medium">{writeHeadline(write)}</span>
        <StatusPill tone={WRITE_STATE[write.state].tone}>{WRITE_STATE[write.state].label}</StatusPill>
        {write.attempts > 1 && <span className="text-xs text-ink-2">{write.attempts} attempts</span>}
        <span className="ml-auto flex gap-1">
          <Button variant="ghost" aria-expanded={shown} aria-controls={detailsId} onClick={() => setShown((s) => !s)}>
            {shown ? 'Hide' : 'What it sends'}
          </Button>
          {person && write.state === 'failed' && (
            <Button disabled={retry.isPending} onClick={() => retry.mutate(write.proposal.ask)}>
              Retry
            </Button>
          )}
        </span>
      </div>
      <Outcome write={write} />
      {shown && (
        <div id={detailsId}>
          <WriteDiff write={write} />
        </div>
      )}
      {retry.error !== null && <ErrorNote error={retry.error} what="retry" />}
    </li>
  );
}

function CommentForm({ task }: { task: Task }) {
  const { request } = useWriteActions();
  const [text, setText] = useState('');
  const labelId = useId();
  const submit = (event: FormEvent) => {
    event.preventDefault();
    const trimmed = text.trim();
    if (trimmed === '') return;
    request.mutate({ task: task.id, operation: 'comment', text: trimmed }, { onSuccess: () => setText('') });
  };
  return (
    <form onSubmit={submit} className="flex flex-col gap-1">
      <label id={labelId} className="text-xs font-medium text-ink-2">
        Comment on {task.source?.key ?? 'the issue'} (sent after you approve it in the Inbox)
      </label>
      <textarea
        aria-labelledby={labelId}
        className={inputClass}
        rows={2}
        value={text}
        onChange={(e) => setText(e.target.value)}
      />
      <div>
        <Button type="submit" disabled={request.isPending || text.trim() === ''}>
          Ask to comment
        </Button>
      </div>
      {request.error !== null && <ErrorNote error={request.error} what="ask to comment" />}
    </form>
  );
}

function CreateIssue({ task }: { task: Task }) {
  const { request } = useWriteActions();
  return (
    <div className="flex flex-col gap-1">
      <div className="flex items-center gap-2">
        <Button
          disabled={request.isPending}
          onClick={() => request.mutate({ task: task.id, operation: 'create_issue' })}
        >
          Create an issue upstream
        </Button>
        {request.data !== undefined && (
          <span role="status" className="text-xs text-ink-2">
            Waiting for approval in the Inbox.
          </span>
        )}
      </div>
      {request.error !== null && <ErrorNote error={request.error} what="ask to create an issue" />}
    </div>
  );
}

/** The drawer's "Upstream" section. Shown when the task mirrors an issue, has writes, or could. */
export function TaskWrites({
  task,
  person,
  section,
}: {
  task: Task;
  person: boolean;
  /** The drawer's own section wrapper, so headings keep its levels. */
  section: (title: string, children: ReactNode) => ReactNode;
}) {
  const writes = useTaskWrites(task.id);
  const integrations = useIntegrations();
  const list = writes.data ?? [];
  const connected = (integrations.data ?? []).length > 0;
  if (task.source === undefined && list.length === 0 && !(person && connected)) return null;
  const pending = list.filter((w) => w.state === 'pending').length;
  return section(
    'Upstream',
    <div className="flex flex-col gap-2">
      {task.source !== undefined && (
        <p className="text-sm text-ink-2">
          Mirrors{' '}
          {task.source.url !== undefined && isWebUrl(task.source.url) ? (
            <a href={task.source.url} target="_blank" rel="noreferrer noopener" className="underline">
              {task.source.key}
            </a>
          ) : (
            task.source.key
          )}{' '}
          on {tracker(task.source.system)}. Changes go upstream only after you approve them.
        </p>
      )}
      {writes.error !== null && <ErrorNote error={writes.error} what="load the writes upstream" />}
      {pending > 0 && (
        <p className="text-sm">
          {pending === 1 ? '1 write waits' : `${pending} writes wait`} for approval in the Inbox.
        </p>
      )}
      {list.length > 0 && (
        <ul aria-label="Writes upstream" className="flex flex-col gap-1.5">
          {[...list].reverse().map((write) => (
            <WriteItem key={write.proposal.ask} write={write} person={person} />
          ))}
        </ul>
      )}
      {person && task.source === undefined && connected && <CreateIssue task={task} />}
      {person && task.source !== undefined && <CommentForm task={task} />}
    </div>,
  );
}
