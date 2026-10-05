// One row of a machine check (the first run's Machine check step, and the connect wizard's probe):
// what it is, its status in words and colour, its reason, and room for an action. And the note a
// row's "Install…" fix leaves: where to install the tool from, with the link to copy, since the
// desktop app opens no browser window itself.

import { useState, type ReactNode } from 'react';
import { StatusPill } from '../design/index.ts';
import type { CheckRowStatus, MachineCheckRow } from './api.ts';
import { installPage } from './install-pages.ts';

const TONE: Record<CheckRowStatus, 'ok' | 'warn' | 'risk' | 'progress'> = {
  ok: 'ok',
  warn: 'warn',
  missing: 'risk',
  checking: 'progress',
};

const STATUS_WORD: Record<CheckRowStatus, string> = {
  ok: 'OK',
  warn: 'Needs attention',
  missing: 'Missing',
  checking: 'Checking…',
};

export function CheckRowLine({ row, note, action }: { row: MachineCheckRow; note?: ReactNode; action?: ReactNode }) {
  return (
    <li className="flex items-center justify-between gap-3 px-3 py-2.5">
      <div className="min-w-0">
        <p className="text-sm text-ink">{row.label}</p>
        {row.detail !== undefined && <p className="text-xs break-words text-ink-2">{row.detail}</p>}
        {note}
      </div>
      <div className="flex shrink-0 items-center gap-2">
        <StatusPill tone={TONE[row.status]}>{STATUS_WORD[row.status]}</StatusPill>
        {action}
      </div>
    </li>
  );
}

/** Where to install `row`'s tool from: its page from this app's own table, with a copy button. */
export function InstallPageNote({ row }: { row: MachineCheckRow }) {
  const page = installPage(row.id);
  const [copied, setCopied] = useState(false);
  if (page === undefined) return null;
  const copy = () => {
    void navigator.clipboard?.writeText(page).then(
      () => setCopied(true),
      () => undefined,
    );
  };
  return (
    <p className="mt-0.5 flex flex-wrap items-center gap-x-2 text-xs text-ink-2">
      <span>
        Install it from <span className="font-mono break-all">{page}</span>, then check again.
      </span>
      <button
        type="button"
        onClick={copy}
        aria-label={`Copy the install page of ${row.label}`}
        className="text-accent-text underline underline-offset-2"
      >
        {copied ? 'Copied' : 'Copy link'}
      </button>
    </p>
  );
}
