// The Orchestrator panel (Ctrl J): resizable, with its open state and width persisted. Its
// conversation (`orchestrator-chat.tsx`) is a lazy chunk, loaded when the panel first opens.

import { lazy, Suspense } from 'react';
import { CloseIcon, FOCUS_RING, ResizablePanel, SparkleIcon, Tooltip } from '../design/index.ts';
import { cx } from '../lib/cx.ts';
import { SHORTCUTS } from './shortcuts.ts';
import { ORCHESTRATOR_WIDTH, useShell } from './store.ts';

const OrchestratorChat = lazy(() => import('./orchestrator-chat.tsx').then((m) => ({ default: m.OrchestratorChat })));

export function OrchestratorPanel() {
  const width = useShell((s) => s.orchestratorWidth);
  const setWidth = useShell((s) => s.setOrchestratorWidth);
  const setOpen = useShell((s) => s.setOrchestratorOpen);
  return (
    <ResizablePanel
      label="Orchestrator"
      width={width}
      onWidthChange={setWidth}
      min={ORCHESTRATOR_WIDTH.min}
      max={ORCHESTRATOR_WIDTH.max}
      className="border-l border-line bg-card"
    >
      <div className="flex h-12 shrink-0 items-center gap-2 border-b border-line px-3">
        <SparkleIcon className="text-accent" />
        <h2 className="text-sm font-semibold">Orchestrator</h2>
        <Tooltip content="Close" keys={SHORTCUTS.orchestrator}>
          <button
            type="button"
            aria-label="Close the Orchestrator"
            onClick={() => setOpen(false)}
            className={cx(
              'ml-auto inline-flex size-7 items-center justify-center rounded-sm text-ink-2 outline-none hover:bg-hover hover:text-ink',
              FOCUS_RING,
            )}
          >
            <CloseIcon />
          </button>
        </Tooltip>
      </div>
      <Suspense fallback={<p className="p-4 text-sm text-ink-2">Loading…</p>}>
        <OrchestratorChat />
      </Suspense>
    </ResizablePanel>
  );
}
