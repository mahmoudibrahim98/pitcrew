// The Orchestrator panel frame (Ctrl J): resizable, with its open state and width persisted. Its
// conversation comes later; for now it shows an empty state.

import { CloseIcon, ResizablePanel, SparkleIcon, Tooltip } from '../design/index.ts';
import { SHORTCUTS } from './shortcuts.ts';
import { ORCHESTRATOR_WIDTH, useShell } from './store.ts';

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
            className="ml-auto inline-flex size-7 items-center justify-center rounded-sm text-ink-2 outline-none hover:bg-hover hover:text-ink focus-visible:outline-2 focus-visible:outline-accent"
          >
            <CloseIcon />
          </button>
        </Tooltip>
      </div>
      <div className="flex flex-1 flex-col items-center justify-center gap-2 p-6 text-center">
        <span className="inline-flex size-10 items-center justify-center rounded-pill bg-accent-soft text-accent-text">
          <SparkleIcon className="size-5" />
        </span>
        <p className="text-sm font-medium">Ask about your work</p>
        <p className="max-w-64 text-sm text-ink-2">
          The Orchestrator will answer questions across your projects, sessions and machines. Its
          conversation arrives in a later version.
        </p>
      </div>
    </ResizablePanel>
  );
}
