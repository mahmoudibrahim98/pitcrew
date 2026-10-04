// Step 10: the exact diff, for every file the hooks touch, before anything is installed
// (`docs/build/streams/O.md`: "hooks (diff first)"). Hooks are fire-and-forget and under 10 ms
// (ADR-0010); installing them here only writes the CLI config that calls out to the daemon.

import { hookDiff } from '../hook-diff.ts';
import { useEffect, useState } from 'react';
import { useOnboardingApi } from '../api-context.tsx';
import { StepFooter } from '../step-footer.tsx';
import { useWizard } from '../wizard-context.tsx';

export function HooksStep() {
  const { state, patch, next, skip } = useWizard();
  const api = useOnboardingApi();
  const [error, setError] = useState<string>();
  const [loading, setLoading] = useState(false);
  const [installing, setInstalling] = useState(false);

  // Cached: `state.hooksDiff` stays set once fetched, so revisiting the step after Back/Forward
  // shows the same diff instead of re-fetching it.
  useEffect(() => {
    if (state.hooksDiff !== undefined) return;
    let active = true;
    void api.hooksDiff().then((diff) => { if (active) patch({ hooksDiff: diff }); })
      .catch((e: unknown) => { if (active) setError(e instanceof Error ? e.message : String(e)); });
    return () => { active = false; };
  }, [api, state.hooksDiff, patch]);

  async function install() {
    if (state.hooksDiff === undefined || state.hooksDiff.files.length === 0) return;
    setError(undefined);
    setInstalling(true);
    try {
      await api.installHooks(state.hooksDiff);
      patch({ hooksInstalled: true });
      next();
    } catch (e) {
      setError(e instanceof Error ? e.message : String(e));
    } finally {
      setInstalling(false);
    }
  }

  return (
    <form
      onSubmit={(e) => {
        e.preventDefault();
        void install();
      }}
    >
      {error !== undefined && <p role="alert">{error}</p>}
      {error !== undefined && <button type="button" disabled={loading} onClick={() => {
        setLoading(true);
        void api.hooksDiff().then((diff) => { patch({ hooksDiff: diff }); setError(undefined); })
          .catch((e: unknown) => setError(e instanceof Error ? e.message : String(e)))
          .finally(() => setLoading(false));
      }}>Refresh diff</button>}
      {state.hooksDiff?.engines.filter((engine) => engine.status === 'conflicting').map((engine) => <p role="alert" key={engine.engine}>{engine.engine}: {engine.detail}</p>)}
      {state.hooksDiff?.engines.length === 0 && <p>No supported agent CLIs were found on this hub.</p>}
      {state.hooksDiff === undefined && <p className="text-sm text-ink-2">Preparing the diff…</p>}
      <ul className="flex flex-col gap-3">
        {state.hooksDiff?.files.map((file) => (
          <li key={file.path} className="rounded-sm border border-line p-3">
            <p className="text-sm font-medium text-ink">{file.path}</p>
            <pre tabIndex={0} aria-label={`Diff: ${file.path}`} className="mt-1.5 overflow-auto rounded-sm bg-card p-2 text-xs text-ink">
              {hookDiff(file.path, file.before, file.after)}
            </pre>
          </li>
        ))}
      </ul>
      <StepFooter
        nextLabel="Install hooks"
        nextDisabled={state.hooksDiff === undefined || loading || state.hooksDiff.files.length === 0}
        onSkip={skip}
        skipLabel="Skip hooks"
        busy={installing}
      />
    </form>
  );
}
