// A unified diff with line numbers and the changed words marked.

import type { ReactNode } from 'react';
import { cx } from '../../lib/cx.ts';
import { useParsed } from './client.ts';
import type { DiffLine, DiffLineType } from './diff-parse.ts';

const LINE: Record<DiffLineType, string> = {
  add: 'bg-ok-soft',
  del: 'bg-risk-soft',
  ctx: '',
  hunk: 'bg-sunken text-accent-text',
  file: 'text-ink-2',
  note: 'text-ink-2 italic',
};

const SIGN: Record<DiffLineType, string> = { add: '+', del: '-', ctx: ' ', hunk: '', file: '', note: '' };

function withMarks(line: DiffLine): ReactNode {
  if (line.marks === undefined || line.marks.length === 0) return line.text;
  const parts: ReactNode[] = [];
  let at = 0;
  for (const [start, end] of line.marks) {
    if (start > at) parts.push(line.text.slice(at, start));
    parts.push(
      <mark key={start} className={cx('rounded-sm text-inherit', line.type === 'add' ? 'bg-ok/25' : 'bg-risk/25')}>
        {line.text.slice(start, end)}
      </mark>,
    );
    at = end;
  }
  if (at < line.text.length) parts.push(line.text.slice(at));
  return parts;
}

export function DiffView({ diff, label, onLine }: { diff: string; label?: string; onLine?: ((line: number) => void) | undefined }) {
  const parsed = useParsed('diff', diff);
  const frame = 'overflow-x-auto rounded-md border border-line bg-card font-mono text-xs leading-5';
  if (parsed === undefined) {
    return (
      <pre className={cx(frame, 'px-3 py-2')} aria-label={label} data-diff="pending">
        {diff}
      </pre>
    );
  }
  return (
    <div className={frame} role="group" aria-label={label} data-diff="ready">
      <div className="grid min-w-max grid-cols-[auto_auto_auto_1fr]">
        {parsed.lines.map((line, i) =>
          line.type === 'hunk' || line.type === 'file' || line.type === 'note' ? (
            <div key={i} className={cx('col-span-4 px-3 whitespace-pre', LINE[line.type])}>
              {line.text}
            </div>
          ) : (
            <div key={i} className={cx('col-span-4 grid grid-cols-subgrid', LINE[line.type])} data-line={line.type}>
              <span className="px-2 text-right text-ink-2 select-none">{line.old ?? ''}</span>
              <span className="px-2 text-right text-ink-2 select-none">{line.new !== undefined && onLine ? <button className="text-accent-text underline" aria-label={`Open line ${line.new}`} onClick={() => onLine(line.new as number)}>{line.new}</button> : line.new ?? ''}</span>
              <span className="pl-1 text-ink-2 select-none" aria-hidden>
                {SIGN[line.type]}
              </span>
              <span className="pr-3 whitespace-pre">
                <span className="sr-only">{line.type === 'add' ? 'added: ' : line.type === 'del' ? 'removed: ' : ''}</span>
                {withMarks(line)}
              </span>
            </div>
          ),
        )}
      </div>
    </div>
  );
}
