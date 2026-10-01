// Receipts: the evidence behind a claim. Pull requests are web links; the rest open through the
// shell's `openReceipt` when it has one, a transcript otherwise opens its session (`openSession`),
// and the others are plain chips until then.

import type { Receipt } from '../data/index.ts';
import { cx } from '../lib/cx.ts';
import { describeReceipt, isWebUrl, type Names } from './format.ts';
import { useProjectsNav } from './nav.tsx';

const CHIP =
  'inline-flex h-5 max-w-64 items-center gap-1 truncate rounded-sm border border-line bg-sunken px-1.5 font-mono text-xs text-ink-2';

function receiptKey(receipt: Receipt, index: number): string {
  return `${receipt.kind}:${index}`;
}

export function ReceiptChip({ receipt, names }: { receipt: Receipt; names: Names }) {
  const nav = useProjectsNav();
  const { label, title } = describeReceipt(receipt, names);
  if (receipt.kind === 'pull_request' && isWebUrl(receipt.url)) {
    return (
      <a
        href={receipt.url}
        target="_blank"
        rel="noopener noreferrer"
        title={title}
        className={cx(CHIP, 'hover:bg-hover hover:text-ink')}
      >
        {label}
      </a>
    );
  }
  if (nav.openReceipt !== undefined) {
    const open = nav.openReceipt;
    return (
      <button
        type="button"
        title={title}
        onClick={() => open(receipt)}
        className={cx(CHIP, 'hover:bg-hover hover:text-ink')}
      >
        {label}
      </button>
    );
  }
  if (receipt.kind === 'transcript' && nav.openSession !== undefined) {
    const open = nav.openSession;
    const session = receipt.session;
    return (
      <button
        type="button"
        title={title}
        onClick={() => open(session, 'chat')}
        className={cx(CHIP, 'hover:bg-hover hover:text-ink')}
      >
        {label}
      </button>
    );
  }
  return (
    <span title={title} className={CHIP}>
      {label}
    </span>
  );
}

export function Receipts({
  receipts,
  names,
  label = 'Receipts',
  className,
}: {
  receipts: readonly Receipt[];
  names: Names;
  label?: string;
  className?: string;
}) {
  if (receipts.length === 0) return null;
  return (
    <ul aria-label={label} className={cx('flex flex-wrap gap-1.5', className)}>
      {receipts.map((receipt, i) => (
        <li key={receiptKey(receipt, i)}>
          <ReceiptChip receipt={receipt} names={names} />
        </li>
      ))}
    </ul>
  );
}
