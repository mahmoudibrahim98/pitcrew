import type { ReactNode } from 'react';
import { cx } from '../lib/cx.ts';

/** Status colours carry meaning only (see `@pitcrew/tokens`). */
export type Tone = 'neutral' | 'accent' | 'ok' | 'warn' | 'risk' | 'progress';

// The ok, warn and progress colours are too light for small text on their soft backgrounds in the
// light theme (under 4.5:1), so those pills keep ink text and let the dot carry the colour.
const TONES: Record<Tone, { pill: string; dot: string }> = {
  neutral: { pill: 'bg-sunken text-ink-2', dot: 'bg-muted' },
  accent: { pill: 'bg-accent-soft text-accent-text', dot: 'bg-accent' },
  ok: { pill: 'bg-ok-soft text-ink-2', dot: 'bg-ok' },
  warn: { pill: 'bg-warn-soft text-ink-2', dot: 'bg-warn' },
  risk: { pill: 'bg-risk-soft text-risk', dot: 'bg-risk' },
  progress: { pill: 'bg-warn-soft text-ink-2', dot: 'bg-progress' },
};

export function StatusPill({
  tone = 'neutral',
  children,
  className,
}: {
  tone?: Tone;
  children: ReactNode;
  className?: string;
}) {
  const { pill, dot } = TONES[tone];
  return (
    <span
      className={cx(
        'inline-flex h-5 shrink-0 items-center gap-1.5 rounded-pill px-2 text-xs font-medium whitespace-nowrap',
        pill,
        className,
      )}
    >
      <span aria-hidden className={cx('size-1.5 rounded-pill', dot)} />
      {children}
    </span>
  );
}
