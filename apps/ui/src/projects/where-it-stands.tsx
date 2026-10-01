// "Where it stands": a project's or workstream's brief with its receipts. People edit and pin it;
// when the back office proposes a newer version, it shows as a proposal to accept or keep aside.

import { useId, useState, type FormEvent } from 'react';
import { Button, StatusPill } from '../design/index.ts';
import type { BriefTarget } from '../data/index.ts';
import { useAcceptBrief, useBrief, useMe, useNames, useSaveBrief, sameTarget } from './data.ts';
import { formatWhen } from './format.ts';
import { Receipts } from './receipts.tsx';
import { ErrorNote, Field, inputClass } from './ui.tsx';

interface Draft {
  text: string;
  next: string;
  pinned: boolean;
}

export function WhereItStands({ target, title = 'Where it stands' }: { target: BriefTarget; title?: string }) {
  const headingId = useId();
  const { brief: cached, isPending, error } = useBrief(target);
  const me = useMe();
  const names = useNames();
  const save = useSaveBrief();
  const accept = useAcceptBrief();
  const [draft, setDraft] = useState<Draft | null>(null);

  // The hub's answer to my own save or accept is newer than the cache until it refreshes it;
  // between the two, whichever happened more recently.
  const forTarget = (data: typeof save.data) => (data !== undefined && sameTarget(data.target, target) ? data : undefined);
  const savedBySave = forTarget(save.data);
  const savedByAccept = forTarget(accept.data);
  const saved =
    savedBySave === undefined
      ? savedByAccept
      : savedByAccept === undefined || savedBySave.updated >= savedByAccept.updated
        ? savedBySave
        : savedByAccept;
  const brief = saved !== undefined && (cached === undefined || saved.updated > cached.updated) ? saved : cached;
  const person = me.data?.kind === 'human';
  const proposal = brief?.proposal;

  const write = (next: { text: string; next?: string | undefined; pinned: boolean }, done?: () => void) => {
    const body =
      next.next === undefined || next.next.trim() === ''
        ? { target, text: next.text, pinned: next.pinned }
        : { target, text: next.text, next: next.next, pinned: next.pinned };
    save.mutate(body, { onSuccess: () => done?.() });
  };

  const submit = (e: FormEvent) => {
    e.preventDefault();
    if (draft === null || draft.text.trim() === '') return;
    write({ text: draft.text.trim(), next: draft.next.trim(), pinned: draft.pinned }, () => setDraft(null));
  };

  return (
    <section aria-labelledby={headingId} className="rounded-md border border-line bg-card p-4">
      <header className="mb-2 flex flex-wrap items-center gap-2">
        <h2 id={headingId} className="text-lg font-semibold">
          {title}
        </h2>
        {brief?.pinned === true && <StatusPill tone="accent">Pinned</StatusPill>}
        {person && brief !== undefined && draft === null && (
          <span className="ml-auto flex gap-1.5">
            <Button
              variant="ghost"
              onClick={() => setDraft({ text: brief.text, next: brief.next ?? '', pinned: brief.pinned })}
            >
              Edit
            </Button>
            <Button
              variant="ghost"
              aria-pressed={brief.pinned}
              disabled={save.isPending}
              onClick={() => write({ text: brief.text, next: brief.next, pinned: !brief.pinned })}
            >
              {brief.pinned ? 'Unpin' : 'Pin'}
            </Button>
          </span>
        )}
      </header>

      {error !== null && <ErrorNote error={error} what="load this summary" />}
      {isPending && error === null && <p className="text-sm text-ink-2">Loading…</p>}

      {draft !== null ? (
        <form onSubmit={submit} className="flex flex-col gap-3">
          <Field label="Where it stands">
            {(fieldId) => (
              <textarea
                id={fieldId}
                required
                rows={4}
                value={draft.text}
                onChange={(e) => setDraft({ ...draft, text: e.target.value })}
                className={inputClass}
              />
            )}
          </Field>
          <Field label="Next step">
            {(fieldId) => (
              <input
                id={fieldId}
                value={draft.next}
                onChange={(e) => setDraft({ ...draft, next: e.target.value })}
                className={inputClass}
              />
            )}
          </Field>
          <label className="flex items-center gap-2 text-sm">
            <input
              type="checkbox"
              checked={draft.pinned}
              onChange={(e) => setDraft({ ...draft, pinned: e.target.checked })}
            />
            Pin it (the back office may then only propose changes)
          </label>
          <div className="flex gap-2">
            <Button type="submit" variant="primary" disabled={save.isPending}>
              Save
            </Button>
            <Button onClick={() => setDraft(null)}>Cancel</Button>
          </div>
        </form>
      ) : (
        brief !== undefined && (
          <div className="flex flex-col gap-2">
            <p className="text-md leading-relaxed">{brief.text}</p>
            {brief.next !== undefined && (
              <p className="text-sm">
                <span className="font-medium">Next: </span>
                {brief.next}
              </p>
            )}
            <Receipts receipts={brief.receipts} names={names} />
            <p className="text-xs text-ink-2">
              {brief.source === 'person' ? 'Written by a person' : 'From the back office'} · updated{' '}
              {formatWhen(brief.updated)}
            </p>
          </div>
        )
      )}

      {!isPending && error === null && brief === undefined && draft === null && (
        <div className="flex items-center gap-2 text-sm text-ink-2">
          <p>Nothing written yet.</p>
          {person && (
            <Button variant="ghost" onClick={() => setDraft({ text: '', next: '', pinned: false })}>
              Write it
            </Button>
          )}
        </div>
      )}

      {brief !== undefined && proposal !== undefined && draft === null && (
        <div role="group" aria-label="Proposed update" className="mt-3 rounded-sm border border-dashed border-line-2 bg-sunken p-3">
          <p className="mb-1 text-xs font-medium text-ink-2">The back office proposes an update · {formatWhen(proposal.at)}</p>
          <p className="mb-2 text-sm">{proposal.text}</p>
          {proposal.next !== undefined && (
            <p className="mb-2 text-sm">
              <span className="font-medium">Next: </span>
              {proposal.next}
            </p>
          )}
          <Receipts receipts={proposal.receipts} names={names} label="Proposal receipts" className="mb-2" />
          {person && (
            <div className="flex gap-2">
              <Button
                variant="primary"
                disabled={save.isPending || accept.isPending}
                onClick={() =>
                  accept.mutate({
                    target,
                    proposal: { text: proposal.text, ...(proposal.next === undefined ? {} : { next: proposal.next }) },
                    pinned: brief.pinned,
                  })
                }
              >
                Accept
              </Button>
              <Button
                disabled={save.isPending || accept.isPending}
                onClick={() => write({ text: brief.text, next: brief.next, pinned: brief.pinned })}
              >
                Keep current
              </Button>
            </div>
          )}
        </div>
      )}

      {save.error !== null && <ErrorNote error={save.error} what="save" />}
      {accept.error !== null && <ErrorNote error={accept.error} what="accept the proposal" />}
    </section>
  );
}
