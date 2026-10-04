// Step 9: all sessions, a filtered set, or start fresh. Sessions are read in place and never
// moved (ADR-0010); importing is reversible. A dry run always runs before the real import so the
// count on screen matches what "Continue" is about to do.

import { useEffect, useMemo, useState } from 'react';
import type { Engine } from '../../data/index.ts';
import type { ImportMode } from '../api.ts';
import { useOnboardingApi } from '../api-context.tsx';
import { StepFooter } from '../step-footer.tsx';
import { useWizard } from '../wizard-context.tsx';

const ENGINES: Engine[] = ['claude', 'codex', 'opencode'];

const MODE_LABEL: Record<ImportMode, string> = {
  all: 'Import all sessions',
  filtered: 'Import a filtered set',
  none: 'Start fresh (import nothing)',
};

export function ImportStep() {
  const { state, patch, next } = useWizard();
  const api = useOnboardingApi();
  const [busy, setBusy] = useState(false);
  const [preview, setPreview] = useState<{ key: string; count: number }>();
  const [error, setError] = useState<string>();

  const filter = useMemo(() => ({
    mode: state.importMode,
    ...(state.importMode === 'filtered' && state.importSince !== '' ? { since: state.importSince } : {}),
    ...(state.importMode === 'filtered' ? { engines: state.importEngines, folders: state.importFolders.map((f) => f.trim()).filter(Boolean) } : {}),
  }), [state.importMode, state.importSince, state.importEngines, state.importFolders]);
  const filterKey = JSON.stringify(filter);
  const ready = preview?.key === filterKey;

  useEffect(() => {
    let live = true;
    void api.importSessions(filter).then((result) => {
      if (live) {
        setPreview({ key: JSON.stringify(filter), count: result.count });
        setError(undefined);
      }
    }).catch((cause: unknown) => {
      if (live) {
        setPreview(undefined);
        setError(cause instanceof Error ? cause.message : String(cause));
      }
    });
    return () => { live = false; };
  }, [api, filter]);

  function toggleEngine(engine: Engine) {
    patch({
      importEngines: state.importEngines.includes(engine)
        ? state.importEngines.filter((e) => e !== engine)
        : [...state.importEngines, engine],
    });
  }

  async function submit() {
    if (!ready) return;
    setBusy(true);
    try {
      const result = await api.commitImport(filter);
      patch({ importResult: result });
      next();
    } catch (cause) {
      setError(cause instanceof Error ? cause.message : String(cause));
    } finally {
      setBusy(false);
    }
  }

  return (
    <form
      onSubmit={(e) => {
        e.preventDefault();
        void submit();
      }}
    >
      <fieldset className="flex flex-col gap-1.5">
        <legend className="text-sm font-medium text-ink">What to import</legend>
        {(['all', 'filtered', 'none'] as const).map((mode) => (
          <label key={mode} className="flex items-center gap-2 text-sm text-ink">
            <input
              type="radio"
              name="import-mode"
              checked={state.importMode === mode}
              onChange={() => patch({ importMode: mode })}
            />
            {MODE_LABEL[mode]}
          </label>
        ))}
      </fieldset>

      {state.importMode === 'filtered' && (
        <div className="mt-3 flex flex-col gap-3 rounded-sm border border-line p-3">
          <div className="flex flex-col gap-1">
            <label htmlFor="import-since" className="text-xs font-medium text-ink-2">
              Only since
            </label>
            <input
              id="import-since"
              type="date"
              value={state.importSince}
              onChange={(e) => patch({ importSince: e.target.value })}
              className="h-7 w-40 rounded-sm border border-line-2 bg-card px-2 text-sm text-ink"
            />
          </div>
          <div className="flex flex-col gap-1">
            <label htmlFor="import-folders" className="text-xs font-medium text-ink-2">Folders (one per line)</label>
            <textarea id="import-folders" value={state.importFolders.join('\n')}
              onChange={(e) => patch({ importFolders: e.target.value.split('\n') })}
              className="rounded-sm border border-line-2 bg-card px-2 text-sm text-ink" />
          </div>
          <div className="flex flex-col gap-1">
            <span className="text-xs font-medium text-ink-2">Engines</span>
            <div className="flex gap-3">
              {ENGINES.map((engine) => (
                <label key={engine} className="flex items-center gap-1.5 text-sm text-ink capitalize">
                  <input
                    type="checkbox"
                    checked={state.importEngines.includes(engine)}
                    onChange={() => toggleEngine(engine)}
                  />
                  {engine}
                </label>
              ))}
            </div>
          </div>
        </div>
      )}

      <p className="mt-3 text-sm text-ink-2" aria-live="polite">
        {!ready
          ? 'Counting…'
          : `This will import ${preview.count} session${preview.count === 1 ? '' : 's'}.`}
      </p>

      <p className="mt-2 text-xs text-ink-2">Sessions stay in place. You can change this choice later. Start fresh includes only sessions started after confirmation.</p>
      {error && <p role="alert" className="mt-2 text-sm text-ink">{error}</p>}
      <StepFooter nextLabel="Continue" nextDisabled={!ready} busy={busy} />
    </form>
  );
}
