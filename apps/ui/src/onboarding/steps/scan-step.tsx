// Step 7: streamed progress, then counts per engine, folder and month, plus the suggested projects
// the "Create" step lets the user tick, rename and regroup. A scan that fails (one already
// running on the hub, the hub out of reach) says why and can be tried again; a finished scan is
// shown again, not repeated, when the person comes back to this step.

import { Progress } from 'radix-ui';
import { useEffect, useState } from 'react';
import { Button } from '../../design/index.ts';
import { useOnboardingApi } from '../api-context.tsx';
import { StepFooter } from '../step-footer.tsx';
import { useWizard } from '../wizard-context.tsx';
import { draftsFromScan } from '../wizard-state.ts';

/** Folders listed under "By folder"; the rest are counted in one line. */
const FOLDERS_SHOWN = 8;

export function ScanStep() {
  const { state, patch, next } = useWizard();
  const api = useOnboardingApi();
  // Bumped by "Try again" and "Scan again", each of which starts another scan.
  const [attempt, setAttempt] = useState(0);
  // A scan that finished earlier in this run is shown as it was: scanning again on every visit
  // would also reset the Create step's choices. Read once, when the step is shown.
  const [resumed] = useState(() => state.scanStatus === 'done' && state.scanResult !== undefined);
  // No "already started" ref-guard: this effect's cleanup cancels whatever it started, so under
  // React StrictMode's dev-only mount→cleanup→remount it cancels the first stream and starts a
  // fresh one, correctly — a guard here would instead block that second, real start and leave the
  // step stuck on its first line forever (found via the Playwright run, not Vitest, since
  // `render()` there does not simulate StrictMode). The hub's `streamScan` sends nothing for a
  // stream cancelled that early, so the hub sees one scan.
  useEffect(() => {
    if (resumed && attempt === 0) return;
    patch({ scanStatus: 'running', scanProgress: undefined, scanError: undefined });
    const streamed = api.streamScan({ machine: state.primaryMachine }, (event) => {
      if (event.type === 'progress') {
        patch({ scanProgress: { scanned: event.scanned, ...(event.total === undefined ? {} : { total: event.total }) } });
      } else if (event.type === 'error') {
        patch({ scanStatus: 'error', scanError: event.message });
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
  }, [api, state.primaryMachine, patch, attempt, resumed]);

  const done = state.scanStatus === 'done';
  const failed = state.scanStatus === 'error';
  const progress = state.scanProgress;
  const percent =
    progress?.total === undefined || progress.total === 0 ? undefined : Math.round((progress.scanned / progress.total) * 100);
  const folders = state.scanResult?.counts.byFolder ?? [];
  const moreFolders = folders.length - FOLDERS_SHOWN;
  const again = () => setAttempt((n) => n + 1);

  return (
    <form
      onSubmit={(e) => {
        e.preventDefault();
        next();
      }}
    >
      {!done && !failed && (
        <div aria-live="polite">
          <p className="text-sm text-ink-2">
            {progress?.total === undefined
              ? 'Scanning this machine’s agent sessions…'
              : `Scanned ${progress.scanned} of ${progress.total} sessions…`}
          </p>
          <Progress.Root
            value={percent ?? null}
            aria-label="Scan progress"
            className="mt-2 h-1.5 overflow-hidden rounded-pill bg-sunken"
          >
            <Progress.Indicator
              className="h-full rounded-pill bg-accent transition-[width]"
              style={{ width: `${percent ?? 15}%` }}
            />
          </Progress.Root>
        </div>
      )}

      {failed && (
        <div className="flex flex-col items-start gap-3">
          <p role="alert" className="text-sm text-risk">
            {state.scanError}
          </p>
          <Button onClick={again}>Try again</Button>
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
            {folders.length === 0 ? (
              <p className="mt-1 text-sm text-ink-2">No agent sessions were found on this machine.</p>
            ) : (
              <ul className="mt-1 text-sm text-ink-2">
                {folders.slice(0, FOLDERS_SHOWN).map((f) => (
                  <li key={f.path} className="flex justify-between gap-3">
                    <span className="truncate">{f.path}</span>
                    <span>{f.count}</span>
                  </li>
                ))}
                {moreFolders > 0 && (
                  <li>
                    and {moreFolders} more {moreFolders === 1 ? 'folder' : 'folders'}
                  </li>
                )}
              </ul>
            )}
          </div>
          <p className="text-sm text-ink-2">
            Found {state.scanResult.suggestedProjects.length} likely{' '}
            {state.scanResult.suggestedProjects.length === 1 ? 'project' : 'projects'} — the next step lets you
            tick, rename or skip them.
          </p>
          <div>
            <Button onClick={again}>Scan again</Button>
          </div>
        </div>
      )}

      <StepFooter nextLabel="Continue" nextDisabled={!done} />
    </form>
  );
}
