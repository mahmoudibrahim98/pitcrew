// Step 7: streamed progress, then counts per engine, folder and month, plus the suggested projects
// the "Create" step lets the user tick, rename and regroup.

import { Progress } from 'radix-ui';
import { useEffect } from 'react';
import { useOnboardingApi } from '../api-context.tsx';
import { StepFooter } from '../step-footer.tsx';
import { useWizard } from '../wizard-context.tsx';
import { draftsFromScan } from '../wizard-state.ts';

export function ScanStep() {
  const { state, patch, next } = useWizard();
  const api = useOnboardingApi();
  // No "already started" ref-guard: this effect's cleanup cancels whatever it started, so under
  // React StrictMode's dev-only mount→cleanup→remount it cancels the first stream and starts a
  // fresh one, correctly — a guard here would instead block that second, real start and leave the
  // step stuck on "Starting the scan…" forever (found via the Playwright run, not Vitest, since
  // `render()` there does not simulate StrictMode).
  useEffect(() => {
    patch({ scanStatus: 'running', scanProgress: undefined });
    const streamed = api.streamScan({ machine: state.primaryMachine }, (event) => {
      if (event.type === 'progress') {
        patch({ scanProgress: { scanned: event.scanned, ...(event.total === undefined ? {} : { total: event.total }) } });
      } else {
        const { projects, workstreams } = draftsFromScan(event.result);
        patch({
          scanStatus: 'done',
          scanResult: event.result,
          createProjects: projects,
          createWorkstreams: workstreams,
        });
      }
    });
    return () => streamed.cancel();
  }, [api, state.primaryMachine, patch]);

  const done = state.scanStatus === 'done';
  const progress = state.scanProgress;
  const percent =
    progress?.total === undefined || progress.total === 0 ? undefined : Math.round((progress.scanned / progress.total) * 100);

  return (
    <form
      onSubmit={(e) => {
        e.preventDefault();
        next();
      }}
    >
      {!done && (
        <div aria-live="polite">
          <p className="text-sm text-ink-2">
            {progress === undefined
              ? 'Starting the scan…'
              : `Scanned ${progress.scanned}${progress.total === undefined ? '' : ` of ${progress.total}`} sessions…`}
          </p>
          <Progress.Root value={percent ?? null} className="mt-2 h-1.5 overflow-hidden rounded-pill bg-sunken">
            <Progress.Indicator
              className="h-full rounded-pill bg-accent transition-[width]"
              style={{ width: `${percent ?? 15}%` }}
            />
          </Progress.Root>
        </div>
      )}

      {done && state.scanResult !== undefined && (
        <div className="flex flex-col gap-4">
          <div className="grid grid-cols-3 gap-3">
            {Object.entries(state.scanResult.counts.byEngine).map(([engine, count]) => (
              <div key={engine} className="rounded-sm border border-line px-3 py-2">
                <p className="text-xs text-ink-2 capitalize">{engine}</p>
                <p className="text-lg font-semibold text-ink">{count}</p>
              </div>
            ))}
          </div>
          <div>
            <p className="text-sm font-medium text-ink">By folder</p>
            <ul className="mt-1 text-sm text-ink-2">
              {state.scanResult.counts.byFolder.map((f) => (
                <li key={f.path} className="flex justify-between">
                  <span className="truncate">{f.path}</span>
                  <span>{f.count}</span>
                </li>
              ))}
            </ul>
          </div>
          <p className="text-sm text-ink-2">
            Found {state.scanResult.suggestedProjects.length} likely{' '}
            {state.scanResult.suggestedProjects.length === 1 ? 'project' : 'projects'} — the next step lets you
            tick, rename or skip them.
          </p>
        </div>
      )}

      <StepFooter nextLabel="Continue" nextDisabled={!done} />
    </form>
  );
}
