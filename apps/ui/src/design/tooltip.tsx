import { Tooltip as RadixTooltip } from 'radix-ui';
import type { ReactElement, ReactNode } from 'react';
import { Kbd } from './kbd.tsx';

/** Once, near the root. */
export function TooltipProvider({ children }: { children: ReactNode }) {
  return (
    <RadixTooltip.Provider delayDuration={400} skipDelayDuration={200}>
      {children}
    </RadixTooltip.Provider>
  );
}

/**
 * A label on hover and keyboard focus. It supplements the control's own accessible name; it is
 * never the only name an icon button has.
 */
export function Tooltip({
  content,
  keys,
  side = 'bottom',
  children,
}: {
  content: ReactNode;
  /** A shortcut shown after the label, e.g. `['mod', 'k']`. */
  keys?: readonly string[];
  side?: 'top' | 'right' | 'bottom' | 'left';
  children: ReactElement;
}) {
  return (
    <RadixTooltip.Root>
      <RadixTooltip.Trigger asChild>{children}</RadixTooltip.Trigger>
      <RadixTooltip.Portal>
        <RadixTooltip.Content
          side={side}
          sideOffset={6}
          className="z-50 flex items-center gap-2 rounded-sm bg-ink px-2 py-1 text-xs text-bg shadow-pop"
        >
          {content}
          {keys !== undefined && <Kbd keys={keys} className="opacity-90" />}
        </RadixTooltip.Content>
      </RadixTooltip.Portal>
    </RadixTooltip.Root>
  );
}
