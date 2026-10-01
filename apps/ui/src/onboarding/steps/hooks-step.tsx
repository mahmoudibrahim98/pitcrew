// Step 10: the exact diff, for every file the hooks touch, before anything is installed
// (`docs/build/streams/O.md`: "hooks (diff first)"). Hooks are fire-and-forget and under 10 ms
// (ADR-0010); installing them here only writes the CLI config that calls out to the daemon.

import { useEffect, useRef, useState } from 'react';
import { useOnboardingApi } from '../api-context.tsx';
import { StepFooter } from '../step-footer.tsx';
import { useWizard } from '../wizard-context.tsx';

export function HooksStep() {
  const { state, patch, next, skip } = useWizard();
  const api = useOnboardingApi();
  const [installing, setInstalling] = useState(false);
  const loaded = useRef(false);

  useEffect(() => {
    if (loaded.current) return;
    loaded.current = true;
    void api.hooksDiff().then((diff) => patch({ hooksDiff: diff }));
  }, [api, patch]);

  async function install() {
    setInstalling(true);
    try {
      await api.installHooks();
      patch({ hooksInstalled: true });
      next();
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
      {state.hooksDiff === undefined && <p className="text-sm text-ink-2">Preparing the diff…</p>}
      <ul className="flex flex-col gap-3">
        {state.hooksDiff?.files.map((file) => (
          <li key={file.path} className="rounded-sm border border-line p-3">
            <p className="text-sm font-medium text-ink">{file.path}</p>
            {file.before === null ? (
              <p className="mt-1 text-xs text-ink-2">New file.</p>
            ) : (
              <pre className="mt-1.5 overflow-auto rounded-sm bg-risk-soft p-2 text-xs text-ink">
                {file.before
                  .split('\n')
                  .map((line) => `- ${line}`)
                  .join('\n')}
              </pre>
            )}
            <pre className="mt-1.5 overflow-auto rounded-sm bg-ok-soft p-2 text-xs text-ink">
              {file.after
                .split('\n')
                .map((line) => `+ ${line}`)
                .join('\n')}
            </pre>
          </li>
        ))}
      </ul>
      <StepFooter
        nextLabel="Install hooks"
        nextDisabled={state.hooksDiff === undefined}
        onSkip={skip}
        skipLabel="Skip hooks"
        busy={installing}
      />
    </form>
  );
}
