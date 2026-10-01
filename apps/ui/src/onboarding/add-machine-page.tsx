// The add-a-machine wizard at `/w/$ws/onboarding/add-machine`, reached from the palette ("Add a
// machine") once a workspace already exists. Reuses the same step components as the first-run
// wizard, in the shorter order `steps.ts` defines for `mode: 'add-machine'`.

import { OnboardingApiProvider } from './api-context.tsx';
import { WizardProvider } from './wizard-context.tsx';
import { WizardShell } from './wizard-shell.tsx';

export function AddMachinePage() {
  return (
    <OnboardingApiProvider>
      <WizardProvider mode="add-machine">
        <WizardShell />
      </WizardProvider>
    </OnboardingApiProvider>
  );
}
