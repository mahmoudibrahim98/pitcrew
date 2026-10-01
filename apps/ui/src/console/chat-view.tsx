// The chat view of one session: the transcript, newest page first, virtualised. It opens at the
// end and follows new items while scrolled to the end; scrolling near the top loads the page
// before, and the rows in view stay where they are (the list is anchored to its end).

import { useVirtualizer } from '@tanstack/react-virtual';
import { useCallback, useEffect, useLayoutEffect, useMemo, useRef, useState } from 'react';
import { ApiError, useAsks, useSession, type TranscriptItemOf } from '../data/index.ts';
import { cx } from '../lib/cx.ts';
import { ChatRowView, PlanChecklist, type RowContext } from './chat-rows.tsx';
import { usePending, useTranscript } from './data.ts';
import { askForQuestion, buildRows, latestPlan, type ChatRowType } from './transcript.ts';

/** First guesses at row heights; rows are measured once drawn. */
const ESTIMATE: Record<ChatRowType, number> = {
  start: 32,
  gap: 32,
  prompt: 72,
  pending: 72,
  text: 96,
  tool: 40,
  edit: 40,
  plan: 120,
  question: 150,
  turn: 32,
};

/** How close to the top (px) a scroll loads the page before. */
const LOAD_OLDER_WITHIN = 400;
/** How close to the end (px) counts as being at the end, which new rows then follow. */
const STAY_AT_END_WITHIN = 48;

export interface ChatViewProps {
  sessionId: string;
  /** Items per page; the API's default when absent. */
  pageSize?: number;
  className?: string;
}

export function ChatView(props: ChatViewProps) {
  // A session's window, scroll position and expanded rows are its own.
  return <ChatViewBody key={props.sessionId} {...props} />;
}

function ChatViewBody({ sessionId, pageSize, className }: ChatViewProps) {
  'use no memo'; // TanStack Virtual's instance changes under the React Compiler's memoisation.
  const transcript = useTranscript(sessionId, { pageSize });
  const { view } = transcript;
  const session = useSession(sessionId);
  const asks = useAsks();
  const pending = usePending(sessionId, view);
  const rows = useMemo(() => buildRows(view, pending), [view, pending]);
  const plan = useMemo(() => latestPlan(view.items), [view.items]);

  const [expanded, setExpanded] = useState<ReadonlySet<string>>(() => new Set());
  const toggle = useCallback((key: string) => {
    setExpanded((current) => {
      const next = new Set(current);
      if (!next.delete(key)) next.add(key);
      return next;
    });
  }, []);
  const askFor = useCallback(
    (question: TranscriptItemOf<'question'>) => askForQuestion(asks.data, sessionId, question),
    [asks.data, sessionId],
  );
  const ctx: RowContext = {
    session: session.data,
    askFor,
    asksKnown: asks.data !== undefined || asks.isError,
    expanded,
    toggle,
  };

  const scrollRef = useRef<HTMLDivElement>(null);
  // eslint-disable-next-line react-hooks/incompatible-library -- this component opts out of the compiler ('use no memo').
  const virtualizer = useVirtualizer({
    count: rows.length,
    getScrollElement: () => scrollRef.current,
    estimateSize: (i) => ESTIMATE[rows[i]?.type ?? 'text'],
    getItemKey: (i) => rows[i]?.key ?? i,
    overscan: 6,
    paddingStart: 8,
    paddingEnd: 8,
    anchorTo: 'end',
    followOnAppend: true,
    scrollEndThreshold: STAY_AT_END_WITHIN,
  });

  // Open at the end, once the first rows are there.
  const opened = useRef(false);
  useLayoutEffect(() => {
    if (!opened.current && rows.length > 0) {
      opened.current = true;
      virtualizer.scrollToEnd();
    }
  }, [rows.length, virtualizer]);

  // At the end, stay there when the view itself shrinks or grows (the plan bar appears, the
  // composer grows); the virtualiser only follows rows that change.
  const atEnd = useRef(true);
  useEffect(() => {
    const element = scrollRef.current;
    if (element === null || typeof ResizeObserver !== 'function') return;
    let height = element.clientHeight;
    const observer = new ResizeObserver(() => {
      if (element.clientHeight === height) return;
      height = element.clientHeight;
      if (opened.current && atEnd.current) virtualizer.scrollToEnd();
    });
    observer.observe(element);
    return () => observer.disconnect();
  }, [virtualizer]);

  const { loadOlder, loadingOlder } = transcript;
  // A page that does not fill the view (or still ends near its top) cannot be scrolled up to load
  // the one before, so load it now.
  useEffect(() => {
    const element = scrollRef.current;
    if (element !== null && opened.current && !loadingOlder && element.scrollTop < LOAD_OLDER_WITHIN) loadOlder();
  }, [view, loadingOlder, loadOlder]);

  const onScroll = () => {
    const element = scrollRef.current;
    if (element === null) return;
    atEnd.current = element.scrollHeight - element.clientHeight - element.scrollTop <= STAY_AT_END_WITHIN;
    if (opened.current && element.scrollTop < LOAD_OLDER_WITHIN) loadOlder();
  };

  const s = session.data;
  const error = transcript.error;
  return (
    <section
      aria-label="Chat"
      className={cx('flex h-full min-h-0 flex-col', className)}
      data-loaded={view.loaded}
    >
      {plan !== undefined && <PlanBar plan={plan} />}
      <div className="relative min-h-0 flex-1">
        {/* Focusable but not a tab stop: the key that moves between panes lands here, and the
            arrow and Page keys then scroll the transcript. */}
        <div
          ref={scrollRef}
          data-virtual-scroller=""
          data-chat-scroller=""
          role="group"
          aria-label="Transcript"
          tabIndex={-1}
          onScroll={onScroll}
          className="h-full overflow-y-auto outline-none focus-visible:outline-2 focus-visible:-outline-offset-2 focus-visible:outline-accent focus-visible:outline-solid"
        >
          <div className="relative w-full" style={{ height: virtualizer.getTotalSize() }}>
            {virtualizer.getVirtualItems().map((item) => {
              const row = rows[item.index];
              if (row === undefined) return null;
              return (
                <div
                  key={item.key}
                  data-index={item.index}
                  data-row={row.type}
                  ref={virtualizer.measureElement}
                  className="absolute top-0 left-0 w-full px-4 py-1.5"
                  style={{ transform: `translateY(${item.start}px)` }}
                >
                  <ChatRowView row={row} ctx={ctx} />
                </div>
              );
            })}
          </div>
        </div>
        {view.loaded && !view.atStart && (
          <div className="pointer-events-none absolute inset-x-0 top-2 flex justify-center">
            <button
              type="button"
              onClick={loadOlder}
              disabled={loadingOlder}
              className="pointer-events-auto rounded-pill border border-line bg-card px-3 py-0.5 text-xs text-ink-2 shadow-pop hover:bg-hover disabled:opacity-70"
            >
              {loadingOlder ? 'Loading earlier…' : 'Load earlier'}
            </button>
          </div>
        )}
        {transcript.isPending && (
          <p className="absolute inset-0 flex items-center justify-center text-sm text-ink-2">Loading the transcript…</p>
        )}
        {view.loaded && rows.length === 0 && (
          <p className="absolute inset-0 flex items-center justify-center text-sm text-ink-2">Nothing here yet.</p>
        )}
      </div>
      {error !== null && (
        <p role="alert" className="mx-4 mb-2 rounded-md bg-risk-soft px-3 py-1.5 text-xs text-risk">
          {error instanceof ApiError ? `Could not load the transcript: ${error.message}` : 'Could not load the transcript.'}
        </p>
      )}
      {s !== undefined && (s.state === 'working' || s.state === 'starting') && (
        <p className="flex items-center gap-2 px-4 pb-2 text-xs text-ink-2" role="status" aria-live="polite">
          <span aria-hidden className="size-1.5 animate-pulse rounded-pill bg-progress" />
          {s.state === 'starting' ? 'Starting…' : (s.status_line ?? 'Working…')}
        </p>
      )}
    </section>
  );
}

function PlanBar({ plan }: { plan: TranscriptItemOf<'plan_updated'> }) {
  const [open, setOpen] = useState(false);
  const done = plan.items.filter((i) => i.status === 'completed').length;
  const current = plan.items.find((i) => i.status === 'in_progress');
  return (
    <div className="border-b border-line bg-card px-4 py-1.5 text-xs">
      <button
        type="button"
        aria-expanded={open}
        onClick={() => setOpen(!open)}
        className="flex w-full min-w-0 items-center gap-2 text-left"
      >
        <span className="font-semibold">Plan</span>
        <span className="text-ink-2 tabular-nums">
          {done}/{plan.items.length}
        </span>
        {current !== undefined && <span className="min-w-0 truncate text-ink-2">{current.text}</span>}
        <span aria-hidden className="ml-auto text-ink-2">
          {open ? 'Hide' : 'Show'}
        </span>
      </button>
      {open && (
        <div className="pt-1.5">
          <PlanChecklist items={plan.items} />
        </div>
      )}
    </div>
  );
}
