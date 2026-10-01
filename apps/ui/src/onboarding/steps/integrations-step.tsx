// Step 6 (first run only): connect external trackers. Entirely skippable; stream G wires the real
// connections later.

import { useEffect, useRef } from 'react';
import type { IntegrationId } from '../api.ts';
import { useOnboardingApi } from '../api-context.tsx';
import { StepFooter } from '../step-footer.tsx';
import { useWizard } from '../wizard-context.tsx';

const LABEL: Record<IntegrationId, string> = {
  github: 'GitHub',
  jira: 'Jira',
  linear: 'Linear',
  gitlab: 'GitLab',
};

export function IntegrationsStep() {
  const { state, patch, next, skip } = useWizard();
  const api = useOnboardingApi();
  const loaded = useRef(false);

  useEffect(() => {
    if (loaded.current) return;
    loaded.current = true;
    void api.integrationStatus().then((integrations) => patch({ integrations }));
  }, [api, patch]);

  return (
    <form
      onSubmit={(e) => {
        e.preventDefault();
        next();
      }}
    >
      <p className="text-sm text-ink-2">Link external trackers so their issues show up beside your tasks.</p>
      <ul className="mt-3 divide-y divide-line rounded-sm border border-line">
        {state.integrations.map((integration) => (
          <li key={integration.id} className="flex items-center justify-between gap-3 px-3 py-2.5">
            <div>
              <p className="text-sm text-ink">{LABEL[integration.id]}</p>
              {integration.detail !== undefined && <p className="text-xs text-ink-2">{integration.detail}</p>}
            </div>
            <button
              type="button"
              onClick={() =>
                patch({
                  integrations: state.integrations.map((i) =>
                    i.id === integration.id ? { ...i, connected: !i.connected } : i,
                  ),
                })
              }
              className="h-6 rounded-sm border border-line-2 px-2 text-xs text-ink-2 hover:bg-hover"
            >
              {integration.connected ? 'Disconnect' : 'Connect'}
            </button>
          </li>
        ))}
      </ul>
      <StepFooter nextLabel="Continue" onSkip={skip} skipLabel="Skip integrations" />
    </form>
  );
}
