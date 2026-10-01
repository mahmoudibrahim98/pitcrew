// A recap's text with its clauses marked. Every clause with receipts is a button: hovering or
// focusing it previews its evidence, activating it (click, Enter or Space) opens the evidence to
// follow — the receipts, and the sessions, tasks and files they lead to — and Escape closes it and
// returns to the clause. The text joining clauses stays plain.
//
// Recap text is untrusted (API v1, "Text is untrusted"): it is rendered as text only, never as
// HTML or markdown, and split with the data layer's `clauses()`, since spans are UTF-8 byte ranges.

import { Popover } from 'radix-ui';
import { Fragment, useEffect, useId, useRef, useState, type KeyboardEvent, type PointerEvent } from 'react';
import { clauses, type RecapBlock, type Receipt, type Summary } from '../data/index.ts';
import { cx } from '../lib/cx.ts';
import { useNames } from './data.ts';
import { useProjectsNav } from './nav.tsx';
import { evidenceFor } from './recap-evidence.ts';
import { Receipts } from './receipts.tsx';
import { MaybeLink } from './ui.tsx';

/** `preview`: shown on hover or focus, not interactive. `open`: activated, focus inside. */
type Mode = 'closed' | 'preview' | 'open';

const HOVER_OPEN_MS = 300;
const HOVER_CLOSE_MS = 200;

const CLAUSE =
  'cursor-pointer rounded-sm underline decoration-accent decoration-dotted decoration-1 underline-offset-4 ' +
  'box-decoration-clone hover:bg-accent-soft';

const receiptCount = (n: number): string => `${n} ${n === 1 ? 'receipt' : 'receipts'}`;

/**
 * `summary` with its clauses marked. `blocks` are the blocks of work it covers (a day's, or the
 * one block a line is about), for the sessions, tasks and files behind each clause; `onEvidence`
 * is told when a clause's evidence is shown, so the caller can load blocks it is still missing.
 * Renders a `div`, since an open clause's evidence sits right after it.
 */
export function SummaryText({
  summary,
  blocks = [],
  onEvidence,
  className,
}: {
  summary: Summary;
  blocks?: readonly RecapBlock[];
  onEvidence?: () => void;
  className?: string;
}) {
  return (
    <div className={className} data-summary="">
      {clauses(summary).map((part, i) =>
        part.receipts.length === 0 ? (
          <Fragment key={`${i}:${part.text}`}>{part.text}</Fragment>
        ) : (
          <Clause
            key={`${i}:${part.text}`}
            text={part.text}
            receipts={part.receipts}
            blocks={blocks}
            {...(onEvidence === undefined ? {} : { onEvidence })}
          />
        ),
      )}
    </div>
  );
}

function Clause({
  text,
  receipts,
  blocks,
  onEvidence,
}: {
  text: string;
  receipts: readonly Receipt[];
  blocks: readonly RecapBlock[];
  onEvidence?: () => void;
}) {
  const [mode, setMode] = useState<Mode>('closed');
  const anchor = useRef<HTMLSpanElement>(null);
  const content = useRef<HTMLDivElement>(null);
  const timer = useRef<ReturnType<typeof setTimeout> | undefined>(undefined);
  // Set while focus goes back to the clause on Escape, so that focus does not preview it again.
  const restoring = useRef(false);
  const id = useId();

  const clearTimer = () => {
    if (timer.current !== undefined) clearTimeout(timer.current);
    timer.current = undefined;
  };
  useEffect(() => {
    const pending = timer;
    return () => clearTimeout(pending.current);
  }, []);

  // Activated: focus goes into the evidence, where Tab moves between its links and receipts. This
  // covers a preview being activated; `onOpenAutoFocus` below, evidence opened straight away (its
  // content mounts a render later than `mode` changes).
  useEffect(() => {
    if (mode === 'open') content.current?.focus();
  }, [mode]);

  const preview = () => {
    clearTimer();
    setMode((m) => (m === 'closed' ? 'preview' : m));
    onEvidence?.();
  };
  const toggle = () => {
    clearTimer();
    setMode((m) => (m === 'open' ? 'closed' : 'open'));
    onEvidence?.();
  };
  const closeSoon = () => {
    clearTimer();
    timer.current = setTimeout(() => {
      setMode((m) => (m === 'preview' && document.activeElement !== anchor.current ? 'closed' : m));
    }, HOVER_CLOSE_MS);
  };
  const onPointerEnter = (e: PointerEvent) => {
    if (e.pointerType === 'touch') return;
    clearTimer();
    timer.current = setTimeout(preview, HOVER_OPEN_MS);
  };
  const onPointerLeave = (e: PointerEvent) => {
    if (e.pointerType !== 'touch') closeSoon();
  };
  const onKeyDown = (e: KeyboardEvent) => {
    if (e.key === 'Enter' || e.key === ' ') {
      e.preventDefault();
      toggle();
    }
  };

  return (
    <Popover.Root open={mode !== 'closed'} onOpenChange={(open) => !open && setMode('closed')}>
      <Popover.Anchor asChild>
        <span
          ref={anchor}
          role="button"
          tabIndex={0}
          // The clause first, as it reads on screen, then what activating it gives.
          aria-label={`${text}, with evidence (${receiptCount(receipts.length)})`}
          aria-haspopup="dialog"
          aria-expanded={mode === 'open'}
          aria-controls={mode === 'open' ? id : undefined}
          data-clause=""
          className={cx(CLAUSE, mode !== 'closed' && 'bg-accent-soft')}
          onClick={toggle}
          onKeyDown={onKeyDown}
          onFocus={() => !restoring.current && preview()}
          onBlur={(e) => {
            if (mode === 'preview' && !(content.current?.contains(e.relatedTarget as Node | null) ?? false)) {
              clearTimer();
              setMode('closed');
            }
          }}
          onPointerEnter={onPointerEnter}
          onPointerLeave={onPointerLeave}
        >
          {text}
        </span>
      </Popover.Anchor>
      <Popover.Content
        ref={content}
        id={id}
        aria-label={`Evidence for “${text}”`}
        side="bottom"
        align="start"
        sideOffset={6}
        collisionPadding={12}
        inert={mode === 'preview'}
        onOpenAutoFocus={(e) => {
          e.preventDefault();
          if (mode === 'open') content.current?.focus();
        }}
        onCloseAutoFocus={(e) => e.preventDefault()}
        onEscapeKeyDown={() => {
          if (mode !== 'open') return;
          restoring.current = true;
          anchor.current?.focus();
          restoring.current = false;
        }}
        onInteractOutside={(e) => {
          // The clause itself is not "outside": its own click toggles.
          if (e.target instanceof Node && anchor.current?.contains(e.target)) e.preventDefault();
        }}
        onPointerEnter={clearTimer}
        onPointerLeave={onPointerLeave}
        className="z-50 flex w-80 max-w-[calc(100vw-2rem)] flex-col gap-2 rounded-md border border-line bg-card p-3 text-sm text-ink shadow-pop outline-none"
      >
        <Evidence text={text} receipts={receipts} blocks={blocks} />
      </Popover.Content>
    </Popover.Root>
  );
}

function Evidence({ text, receipts, blocks }: { text: string; receipts: readonly Receipt[]; blocks: readonly RecapBlock[] }) {
  const names = useNames();
  const nav = useProjectsNav();
  const { sessions, tasks, files } = evidenceFor(receipts, blocks);
  const openSession = nav.openSession;
  const openTask = nav.openTask;
  return (
    <>
      <p className="text-xs font-medium text-ink-2">Evidence</p>
      <p className="leading-snug">“{text}”</p>
      {(sessions.length > 0 || tasks.length > 0) && (
        <ul aria-label="Where it happened" className="flex flex-wrap gap-x-3 gap-y-1">
          {sessions.map((session) => (
            <li key={session} className="min-w-0">
              <span className="text-ink-2">Session </span>
              <MaybeLink
                onOpen={openSession === undefined ? undefined : () => openSession(session, 'chat')}
                className="font-medium"
              >
                {names.session(session)}
              </MaybeLink>
            </li>
          ))}
          {tasks.map((task) => (
            <li key={task}>
              <span className="text-ink-2">Task </span>
              <MaybeLink onOpen={openTask === undefined ? undefined : () => openTask(task)} className="font-mono">
                {names.task(task)}
              </MaybeLink>
            </li>
          ))}
        </ul>
      )}
      {files.length > 0 && (
        <ul aria-label="Files" className="flex flex-col gap-0.5">
          {files.map((file) => (
            <li key={file.path} className="font-mono text-xs break-all">
              {file.path}{' '}
              <span className="text-ink-2">
                +{file.added} −{file.removed}
              </span>
            </li>
          ))}
        </ul>
      )}
      <Receipts receipts={receipts} names={names} />
    </>
  );
}
