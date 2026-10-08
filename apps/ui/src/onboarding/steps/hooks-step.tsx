// Step 10: the exact diff, for every file the hooks touch, before anything is installed
// (`docs/build/streams/O.md`: "hooks (diff first)"). Hooks are fire-and-forget and under 10 ms
// (ADR-0010); installing them here only writes the CLI config that calls out to the daemon.

import { hookDiff } from '../hook-diff.ts';
import { useEffect, useState } from 'react';
import { useOnboardingApi } from '../api-context.tsx';
import { StepFooter } from '../step-footer.tsx';
import { useWizard } from '../wizard-context.tsx';
import { ENGINE_NAMES } from '../../design/index.ts';

/** What installing adds, per engine, for one with none of PitCrew's hooks yet (`missing`). */
const SUMMARIES: Partial<Record<string, string>> = {
  claude: 'Adds 5 hooks to Claude Code: session start, prompt submitted, response finished, session end and notifications.',
  codex: 'Adds a notification hook to Codex when a response finishes.',
  opencode: 'Adds an OpenCode plugin to report session activity to PitCrew.',
};

/** An engine id from the hub as people know it; an id this build does not know stays as sent. */
const engineName = (engine: string) => (ENGINE_NAMES as Partial<Record<string, string>>)[engine] ?? engine;

/**
 * One engine's plan in plain words, by the installer's status (`missing`, `partial`, `installed`,
 * `stale`; `conflicting` shows as an alert instead). The hub's own detail follows wherever the
 * summary alone would hide what is there.
 */
function planOf(engine: { engine: string; status: string; detail: string }): { summary: string; detail?: string } {
  const name = engineName(engine.engine);
  switch (engine.status) {
    case 'installed':
      return { summary: `${name} hooks are already installed.` };
    case 'missing': {
      const summary = SUMMARIES[engine.engine];
      return summary === undefined ? { summary: `Adds hooks to ${name}.`, detail: engine.detail } : { summary };
    }
    case 'partial':
      return { summary: `Some ${name} hooks are installed; this adds the rest.`, detail: engine.detail };
    case 'stale':
      return { summary: `${name} hooks are out of date; this updates them.`, detail: engine.detail };
    default:
      return { summary: `${name}: ${engine.detail}` };
  }
}

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
      {state.hooksDiff?.engines.filter((engine) => engine.status === 'conflicting').map((engine) => <p role="alert" key={engine.engine}>{engineName(engine.engine)}: {engine.detail}</p>)}
      {state.hooksDiff?.engines.length === 0 && <p>No supported agent CLIs were found on this hub.</p>}
      {state.hooksDiff === undefined && <p className="text-sm text-ink-2">Preparing the diff…</p>}
      {state.hooksDiff?.engines.filter((engine) => engine.status !== 'conflicting').map((engine) => {
        const plan = planOf(engine);
        return (
          <div key={engine.engine} className="mb-3 text-sm text-ink-2" data-engine={engine.engine}>
            <p>{plan.summary}</p>
            {plan.detail !== undefined && plan.detail !== '' && <p className="mt-0.5 text-xs">{plan.detail}</p>}
          </div>
        );
      })}
      <ul className="flex flex-col gap-3">
        {state.hooksDiff?.files.map((file) => (
          <li key={file.path} className="rounded-sm border border-line p-3">
            <details>
            <summary className="cursor-pointer break-all text-sm font-medium text-ink">Review changes: <span>{file.path}</span></summary>
            <pre tabIndex={0} aria-label={`Diff: ${file.path}`} className="mt-1.5 overflow-auto rounded-sm bg-card p-2 text-xs text-ink">
              {hookDiff(file.path, file.before, file.after)}
            </pre>
            </details>
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
