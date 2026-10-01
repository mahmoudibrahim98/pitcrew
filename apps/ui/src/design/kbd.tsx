import { cx } from '../lib/cx.ts';
import { keyLabel } from '../lib/platform.ts';

/** A shortcut hint such as Ctrl K (⌘ K on macOS). `mod` is the platform's modifier. */
export function Kbd({ keys, className }: { keys: readonly string[]; className?: string }) {
  return (
    <span className={cx('inline-flex items-center gap-0.5', className)}>
      {keys.map((key) => (
        <kbd
          key={key}
          className="inline-flex h-4.5 min-w-4.5 items-center justify-center rounded-[4px] border border-b-2 border-line-2 bg-card px-1 font-mono text-[10.5px] leading-none text-ink-2"
        >
          {keyLabel(key)}
        </kbd>
      ))}
    </span>
  );
}
