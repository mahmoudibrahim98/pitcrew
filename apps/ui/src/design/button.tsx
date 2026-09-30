import { Slot } from 'radix-ui';
import type { ComponentProps } from 'react';
import { cx } from '../lib/cx.ts';

type Variant = 'primary' | 'secondary' | 'ghost';

const VARIANTS: Record<Variant, string> = {
  primary: 'bg-accent text-on-accent hover:bg-accent-hover',
  secondary: 'bg-card text-ink border border-line-2 hover:bg-hover',
  ghost: 'text-ink-2 hover:bg-hover hover:text-ink',
};

export function Button({
  variant = 'secondary',
  asChild = false,
  className,
  ...props
}: ComponentProps<'button'> & { variant?: Variant; asChild?: boolean }) {
  const Component = asChild ? Slot.Root : 'button';
  return (
    <Component
      className={cx(
        'inline-flex h-7 items-center gap-1.5 rounded-sm px-2.5 text-sm font-medium transition-colors',
        'disabled:pointer-events-none disabled:opacity-50',
        VARIANTS[variant],
        className,
      )}
      {...props}
    />
  );
}
