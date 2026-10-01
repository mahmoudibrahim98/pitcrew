// The session pane's Chat | Terminal switch. The choice lives in the URL (`?view=terminal`, see
// search.ts). Without a terminal the Terminal option stays focusable, so its reason is reachable
// from the keyboard, but choosing it does nothing, and the reason shows beside it.

import { ToggleGroup } from 'radix-ui';
import { useId } from 'react';
import { cx } from '../lib/cx.ts';
import type { SessionView } from './search.ts';

export interface ViewSwitchProps {
  value: SessionView;
  onChange(view: SessionView): void;
  /** Why the terminal cannot be shown; the Terminal option is disabled while this is set. */
  terminalUnavailable?: string | undefined;
}

const ITEM =
  'h-6 rounded-sm px-2.5 text-xs font-medium text-ink-2 data-[state=on]:bg-card data-[state=on]:text-ink data-[state=on]:shadow-sm';

export function ViewSwitch({ value, onChange, terminalUnavailable }: ViewSwitchProps) {
  const reasonId = useId();
  const unavailable = terminalUnavailable !== undefined;
  return (
    <div className="flex shrink-0 flex-wrap items-center gap-x-3 gap-y-1 border-b border-line px-4 py-1.5">
      <ToggleGroup.Root
        type="single"
        value={value}
        onValueChange={(next) => {
          if (next === 'chat' || (next === 'terminal' && !unavailable)) onChange(next);
        }}
        aria-label="Session view"
        className="inline-flex rounded-sm border border-line bg-sunken p-0.5"
      >
        <ToggleGroup.Item value="chat" className={ITEM}>
          Chat
        </ToggleGroup.Item>
        <ToggleGroup.Item
          value="terminal"
          aria-disabled={unavailable ? true : undefined}
          aria-describedby={unavailable ? reasonId : undefined}
          className={cx(ITEM, unavailable && 'cursor-not-allowed opacity-50')}
        >
          Terminal
        </ToggleGroup.Item>
      </ToggleGroup.Root>
      {unavailable && (
        <span id={reasonId} data-testid="terminal-unavailable" className="text-xs text-ink-2">
          {terminalUnavailable}
        </span>
      )}
    </div>
  );
}
