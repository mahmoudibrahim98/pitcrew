// The Inbox: open asks addressed to me, grouped by kind, each answerable in place. The question
// card itself is stream M's (`src/console`): it also answers questions raised live in a
// transcript, so the Inbox reuses it instead of keeping its own. Receipts and the task link are
// the Inbox's own, shown alongside it (the console's card does not render them). An approval for a
// write to GitHub or Jira gets its own card (`writes/approval-card.tsx`): exactly what is sent.

import { Suspense, useEffect, useId, useRef, useState } from 'react';
import type { Ask, AskKind } from '../data/index.ts';
import { QuestionCard } from '../console/index.ts';
import { useInbox, useNames } from './data.ts';
import { ASK_KIND, ASK_KINDS } from './format.ts';
import { useProjectsNav } from './nav.tsx';
import { Receipts } from './receipts.tsx';
import { ErrorNote, MaybeLink } from './ui.tsx';
import { useWriteOf } from './writes/api.ts';
import { ApprovalCard } from './writes/approval-card.tsx';

export function groupAsks(asks: readonly Ask[]): { kind: AskKind; asks: Ask[] }[] {
  return ASK_KINDS.map((kind) => ({
    kind,
    asks: asks.filter((a) => a.kind === kind).sort((a, b) => b.created - a.created),
  })).filter((group) => group.asks.length > 0);
}

export function Inbox({ title = 'Inbox' }: { title?: string }) {
  const headingId = useId();
  const inbox = useInbox();
  const [announcement, setAnnouncement] = useState('');
  const seen = useRef<number | null>(null);
  const asks = inbox.data ?? [];
  const groups = groupAsks(asks);

  // A generic live announcement when the open list shrinks (an ask was answered or withdrawn);
  // the card itself shows which answer went where.
  useEffect(() => {
    if (inbox.data === undefined) return;
    if (seen.current !== null && asks.length < seen.current) {
      setAnnouncement(`${asks.length === 1 ? '1 open ask' : `${asks.length} open asks`} now.`);
    }
    seen.current = asks.length;
  }, [asks.length, inbox.data]);

  return (
    <section aria-labelledby={headingId} className="flex flex-col gap-4">
      <header className="flex items-baseline gap-2">
        <h1 id={headingId} className="text-xl font-semibold">
          {title}
        </h1>
        {inbox.data !== undefined && (
          <span className="text-sm text-ink-2">{asks.length === 1 ? '1 open' : `${asks.length} open`}</span>
        )}
      </header>
      <p role="status" aria-live="polite" className="sr-only">
        {announcement}
      </p>
      {inbox.error !== null && <ErrorNote error={inbox.error} what="load the Inbox" />}
      {inbox.isPending && inbox.error === null && <p className="text-sm text-ink-2">Loading…</p>}
      {inbox.data !== undefined && asks.length === 0 && (
        <p className="text-sm text-ink-2">Nothing needs you right now.</p>
      )}
      {groups.map((group) => (
        <AskGroup key={group.kind} kind={group.kind} asks={group.asks} />
      ))}
    </section>
  );
}

function AskCardFallback() {
  return <div aria-hidden className="h-20 animate-pulse rounded-lg border border-line bg-sunken" />;
}

/**
 * An approval the hub raised for a write upstream shows exactly what will be sent; any other ask,
 * an approval an agent raised itself included, is the console's question card.
 */
function AskBody({ ask }: { ask: Ask }) {
  const write = useWriteOf(ask);
  if (write.data !== undefined) return <ApprovalCard ask={ask} write={write.data} />;
  if (ask.kind === 'approval' && write.error === null) return <AskCardFallback />;
  return (
    <Suspense fallback={<AskCardFallback />}>
      <QuestionCard ask={ask} />
    </Suspense>
  );
}

function AskCard({ ask }: { ask: Ask }) {
  const names = useNames();
  const nav = useProjectsNav();
  const openTask = nav.openTask;
  const task = ask.task;
  return (
    <div data-ask={ask.id} className="flex flex-col gap-1.5">
      <AskBody ask={ask} />
      {(task !== undefined || ask.receipts.length > 0) && (
        <div className="flex flex-wrap items-center gap-2 px-1">
          {task !== undefined && (
            <MaybeLink onOpen={openTask === undefined ? undefined : () => openTask(task)} className="font-mono text-xs text-ink-2">
              {names.task(task)}
            </MaybeLink>
          )}
          <Receipts receipts={ask.receipts} names={names} />
        </div>
      )}
    </div>
  );
}

function AskGroup({ kind, asks }: { kind: AskKind; asks: Ask[] }) {
  const id = useId();
  return (
    <section aria-labelledby={id} className="flex flex-col gap-2">
      <h2 id={id} className="text-md font-semibold">
        {ASK_KIND[kind].many} <span className="text-sm font-normal text-ink-2">{asks.length}</span>
      </h2>
      <ul className="flex flex-col gap-2">
        {asks.map((ask) => (
          <li key={ask.id}>
            <AskCard ask={ask} />
          </li>
        ))}
      </ul>
    </section>
  );
}
