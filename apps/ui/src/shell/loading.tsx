import { ConsoleIcon } from '../design/index.ts';

export function LoadingSplash() {
  return <div role="status" className="flex min-h-64 flex-1 flex-col items-center justify-center gap-3 bg-bg p-8 text-ink">
    <span className="flex size-12 items-center justify-center rounded-lg border border-line bg-card"><ConsoleIcon className="size-6" /></span>
    <span className="text-lg font-semibold">PitCrew</span>
    <span className="text-sm text-ink-2">Opening your workspace…</span>
  </div>;
}
