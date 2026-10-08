// One chat row per kind: prompts, assistant markdown, tool calls with their results, file edits
// with their diffs, plans, questions and turn ends. Everything from the transcript is text.

import type { MouseEvent } from 'react';
import type { Ask, PlanItem, Session, TranscriptItemOf } from '../data/index.ts';
import { cx } from '../lib/cx.ts';
import { clockTime, ENGINE_LABEL, fullTime, sessionTitle, STATE } from './format.ts';
import { QuestionCard } from './question-card.tsx';
import { DiffView } from './render/diff-view.tsx';
import { Markdown } from './render/markdown.tsx';
import type { ChatRow } from './transcript.ts';

export interface RowContext {
  session: Session | undefined;
  askFor: (question: TranscriptItemOf<'question'>) => Ask | undefined;
  /**
   * The asks have loaded. Until then a question is not answerable here: it may have been raised
   * as an ask, and answering it with keys instead would bypass the ask.
   */
  asksKnown: boolean;
  expanded: ReadonlySet<string>;
  toggle: (key: string) => void;
  /** Where a sub-agent's own transcript is, and opening it with a plain click. */
  sessionHref?: ((session: string) => string) | undefined;
  onOpenSession?: ((session: string) => void) | undefined;
}

/** Where a sub-agent ran: its name and state, and a link to its own transcript. */
function SubagentRowView({ session, ctx }: { session: Session; ctx: RowContext }) {
  const title = sessionTitle(session);
  const href = ctx.sessionHref?.(session.id);
  const { onOpenSession } = ctx;
  const follow = (event: MouseEvent<HTMLAnchorElement>) => {
    if (onOpenSession === undefined) return;
    if (event.button !== 0 || event.metaKey || event.ctrlKey || event.shiftKey || event.altKey) return;
    event.preventDefault();
    onOpenSession(session.id);
  };
  return (
    <div className="flex min-w-0 items-center gap-2 rounded-md border border-dashed border-line px-2.5 py-1.5 text-xs" data-subagent={session.id}>
      <span className="shrink-0 font-semibold">Sub-agent</span>
      <span className="min-w-0 truncate">{title}</span>
      <span className="shrink-0 text-ink-2">
        {ENGINE_LABEL[session.engine]} · {STATE[session.state].label}
      </span>
      <Time at={session.started} />
      {href !== undefined && (
        <a href={href} onClick={follow} className="ml-auto shrink-0 text-accent-text underline-offset-2 hover:underline">
          Open its transcript<span className="sr-only">: {title}</span>
        </a>
      )}
    </div>
  );
}

function Time({ at }: { at: number }) {
  return (
    <time dateTime={new Date(at).toISOString()} title={fullTime(at)} className="text-xs text-ink-2">
      {clockTime(at)}
    </time>
  );
}

function inputText(input: unknown): string {
  if (input === undefined) return '';
  if (typeof input === 'string') return input;
  try {
    return JSON.stringify(input, null, 2);
  } catch {
    return String(input);
  }
}

function firstLine(text: string): string {
  const line = text.split('\n', 1)[0] ?? '';
  return line.length > 160 ? `${line.slice(0, 157)}…` : line;
}

function Chevron({ open }: { open: boolean }) {
  return (
    <span aria-hidden className={cx('inline-block w-3 text-ink-2 transition-transform', open && 'rotate-90')}>
      ›
    </span>
  );
}

function ToolRowView({
  row,
  ctx,
}: {
  row: Extract<ChatRow, { type: 'tool' }>;
  ctx: RowContext;
}) {
  const { use, result } = row;
  const open = ctx.expanded.has(row.key);
  const running = result === undefined && ctx.session?.state === 'working';
  const status = result === undefined ? (running ? 'running' : 'no result') : result.is_error ? 'error' : 'ok';
  const detailsId = `pc-tool-${row.key}`;
  return (
    <div className="rounded-md border border-line bg-card text-sm" data-tool={use?.tool ?? 'result'}>
      <button
        type="button"
        aria-expanded={open}
        aria-controls={detailsId}
        onClick={() => ctx.toggle(row.key)}
        className="flex w-full min-w-0 items-center gap-2 px-2.5 py-1.5 text-left hover:bg-hover"
      >
        <Chevron open={open} />
        <span className="shrink-0 font-mono text-xs font-semibold">{use?.tool ?? 'Result'}</span>
        {use !== undefined && use.target !== '' && (
          <span className="min-w-0 shrink truncate font-mono text-xs text-ink-2">{use.target}</span>
        )}
        {/* The progress colour is too light for small text, so it goes in a dot. */}
        <span
          className={cx('ml-auto flex shrink-0 items-center gap-1.5 text-xs', status === 'error' ? 'text-risk' : 'text-ink-2')}
          data-status={status}
        >
          {status === 'running' && <span aria-hidden className="size-1.5 animate-pulse rounded-pill bg-progress" />}
          {status === 'ok' ? firstLine(result?.summary ?? '') || 'done' : status}
        </span>
      </button>
      {open && (
        <div id={detailsId} className="flex flex-col gap-2 border-t border-line px-2.5 py-2">
          {use !== undefined && (
            <div>
              <div className="mb-0.5 text-xs text-ink-2">Input</div>
              <pre className="max-h-80 overflow-auto rounded-sm bg-sunken px-2 py-1.5 font-mono text-xs whitespace-pre-wrap">
                {inputText(use.input) || use.target}
              </pre>
            </div>
          )}
          <div>
            <div className="mb-0.5 text-xs text-ink-2">{result?.is_error ? 'Error' : 'Output'}</div>
            <pre
              className={cx(
                'max-h-80 overflow-auto rounded-sm px-2 py-1.5 font-mono text-xs whitespace-pre-wrap',
                result?.is_error ? 'bg-risk-soft text-risk' : 'bg-sunken',
              )}
            >
              {result?.summary ?? (running ? 'Running…' : 'No result recorded.')}
            </pre>
          </div>
        </div>
      )}
    </div>
  );
}

function EditRowView({ row, ctx }: { row: Extract<ChatRow, { type: 'edit' }>; ctx: RowContext }) {
  const { item } = row;
  const open = ctx.expanded.has(row.key);
  const detailsId = `pc-edit-${row.key}`;
  return (
    <div className="rounded-md border border-line bg-card text-sm">
      <button
        type="button"
        aria-expanded={open}
        aria-controls={detailsId}
        onClick={() => ctx.toggle(row.key)}
        className="flex w-full min-w-0 items-center gap-2 px-2.5 py-1.5 text-left hover:bg-hover"
      >
        <Chevron open={open} />
        <span className="shrink-0 text-xs font-semibold">Edited</span>
        <span className="min-w-0 truncate font-mono text-xs">{item.path}</span>
        <span className="ml-auto shrink-0 font-mono text-xs">
          <span className="text-ok">+{item.added}</span> <span className="text-risk">−{item.removed}</span>
        </span>
      </button>
      {open && (
        <div id={detailsId} className="border-t border-line p-2">
          {item.diff === undefined ? (
            <p className="text-xs text-ink-2">The CLI recorded no diff for this edit.</p>
          ) : (
            <DiffView diff={item.diff} label={`Changes to ${item.path}`} />
          )}
        </div>
      )}
    </div>
  );
}

const PLAN_MARK: Record<PlanItem['status'], { mark: string; label: string; className: string }> = {
  completed: { mark: '✓', label: 'done', className: 'text-ok' },
  in_progress: { mark: '◐', label: 'in progress', className: 'text-progress' },
  pending: { mark: '○', label: 'to do', className: 'text-ink-2' },
};

export function PlanChecklist({ items }: { items: readonly PlanItem[] }) {
  return (
    <ul className="flex flex-col gap-0.5 text-sm">
      {items.map((item, i) => {
        const mark = PLAN_MARK[item.status];
        return (
          <li key={i} className="flex items-start gap-2" data-plan-status={item.status}>
            <span aria-hidden className={cx('w-4 shrink-0 text-center', mark.className)}>
              {mark.mark}
            </span>
            <span className={cx(item.status === 'completed' && 'text-ink-2 line-through decoration-line-2')}>
              <span className="sr-only">{mark.label}: </span>
              {item.text}
            </span>
          </li>
        );
      })}
    </ul>
  );
}

export function ChatRowView({ row, ctx }: { row: ChatRow; ctx: RowContext }) {
  switch (row.type) {
    case 'start':
      return <p className="py-2 text-center text-xs text-ink-2">Start of the transcript</p>;
    case 'gap':
      return (
        <p className="py-2 text-center text-xs text-ink-2" role="status">
          Loading items between these…
        </p>
      );
    case 'prompt':
    case 'pending': {
      const pending = row.type === 'pending';
      const text = pending ? row.prompt.text : row.item.text;
      return (
        <div className={cx('ml-auto flex max-w-[85%] flex-col items-end gap-0.5', pending && 'opacity-70')}>
          <div className="flex items-center gap-2 text-xs text-ink-2">
            <span>Prompt</span>
            {pending ? <span>Sending…</span> : <Time at={row.item.at} />}
          </div>
          <div className="rounded-lg bg-accent-soft px-3 py-2 text-sm break-words whitespace-pre-wrap">{text}</div>
        </div>
      );
    }
    case 'text':
      return <Markdown text={row.item.text} className="text-sm leading-6" />;
    case 'tool':
      return <ToolRowView row={row} ctx={ctx} />;
    case 'edit':
      return <EditRowView row={row} ctx={ctx} />;
    case 'plan':
      return (
        <div className="rounded-md border border-line bg-card px-3 py-2">
          <div className="mb-1 text-xs text-ink-2">Plan updated</div>
          <PlanChecklist items={row.item.items} />
        </div>
      );
    case 'question':
      return ctx.session === undefined ? (
        <p className="text-sm">{row.item.text}</p>
      ) : (
        <QuestionCard
          session={ctx.session}
          question={row.item}
          ask={ctx.askFor(row.item)}
          answer={row.answer}
          open={row.open && ctx.asksKnown}
        />
      );
    case 'turn':
      return (
        <div className="flex items-center gap-2 py-1 text-xs text-ink-2">
          <span className="h-px flex-1 bg-line" />
          <span>Turn ended</span>
          <Time at={row.item.at} />
          <span className="h-px flex-1 bg-line" />
        </div>
      );
    case 'subagent':
      return <SubagentRowView session={row.session} ctx={ctx} />;
  }
}
