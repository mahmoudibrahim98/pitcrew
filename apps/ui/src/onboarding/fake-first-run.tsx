// Development builds only (`?onboarding=fake`, `first-run-page.tsx`): the first-run wizard with
// every step, against the in-memory fake. Its setup touches no hub, so a fresh hub stays fresh.

import { useState } from 'react';
import { OnboardingApiProvider } from './api-context.tsx';
import { createFakeOnboardingApi } from './fake-api.ts';
import { WizardProvider } from './wizard-context.tsx';
import { WizardShell } from './wizard-shell.tsx';

export function FakeFirstRun() {
  const [api] = useState(() => createFakeOnboardingApi());
  return (
    <OnboardingApiProvider api={api}>
      <WizardProvider>
        <p role="note" className="mx-auto max-w-4xl px-8 pt-4 text-xs text-ink-2">
          Development: every step runs against a fake. Nothing here reaches the hub.
        </p>
        <WizardShell />
      </WizardProvider>
    </OnboardingApiProvider>
  );
}
