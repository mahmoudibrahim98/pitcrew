// The Inbox: open asks addressed to me, grouped by kind, each answerable in place.

import { useId, useState } from 'react';
import type { Ask, AskKind } from '../data/index.ts';
import { useInbox } from './data.ts';
import { ASK_KIND, ASK_KINDS } from './format.ts';
import { QuestionCard } from './question-card.tsx';
import { ErrorNote } from './ui.tsx';

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
  const asks = inbox.data ?? [];
  const groups = groupAsks(asks);
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
        <AskGroup
          key={group.kind}
          kind={group.kind}
          asks={group.asks}
          onAnswered={(ask, said) => setAnnouncement(`Answered “${ask.title}”: ${said}.`)}
        />
      ))}
    </section>
  );
}

function AskGroup({
  kind,
  asks,
  onAnswered,
}: {
  kind: AskKind;
  asks: Ask[];
  onAnswered: (ask: Ask, said: string) => void;
}) {
  const id = useId();
  return (
    <section aria-labelledby={id} className="flex flex-col gap-2">
      <h2 id={id} className="text-md font-semibold">
        {ASK_KIND[kind].many} <span className="text-sm font-normal text-ink-2">{asks.length}</span>
      </h2>
      <ul className="flex flex-col gap-2">
        {asks.map((ask) => (
          <li key={ask.id}>
            <QuestionCard ask={ask} onAnswered={onAnswered} />
          </li>
        ))}
      </ul>
    </section>
  );
}
