import type { ReactNode } from 'react';
import { cx } from '../lib/cx.ts';

/**
 * `accent` for things to act on (open asks), `warn` for "needs you", `risk` for failures, `neutral`
 * for plain counts. Text colours meet WCAG AA on every tone, light and dark.
 */
export type BadgeTone = 'accent' | 'warn' | 'risk' | 'neutral' | 'plain';

const TONES: Record<BadgeTone, string> = {
  accent: 'bg-accent text-on-accent font-semibold',
  warn: 'bg-warn text-on-accent font-semibold',
  risk: 'bg-risk text-on-accent font-semibold',
  neutral: 'border border-line bg-sunken text-ink-2',
  plain: 'text-ink-2',
};

/** A small count or tag. `label` is read after the content by screen readers ("3 open asks"). */
export function Badge({
  tone = 'neutral',
  label,
  className,
  children,
  ...rest
}: {
  tone?: BadgeTone;
  label?: string;
  className?: string;
  children: ReactNode;
  'data-testid'?: string;
}) {
  return (
    <span
      className={cx(
        'inline-flex h-4.5 min-w-4.5 shrink-0 items-center justify-center rounded-pill px-1.5 text-[11px] leading-none whitespace-nowrap tabular-nums',
        TONES[tone],
        className,
      )}
      {...rest}
    >
      {children}
      {label !== undefined && <span className="sr-only"> {label}</span>}
    </span>
  );
}
