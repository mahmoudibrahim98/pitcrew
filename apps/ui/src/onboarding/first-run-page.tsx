// The first-run wizard at `/w/$ws/onboarding` (see README.md for why it is not at a pre-workspace
// `/onboarding` today). A lazy route component (`routes.tsx`), so it is its own chunk.

import { OnboardingApiProvider } from './api-context.tsx';
import { WizardProvider } from './wizard-context.tsx';
import { WizardShell } from './wizard-shell.tsx';

export function FirstRunPage() {
  return (
    <OnboardingApiProvider>
      <WizardProvider mode="first-run">
        <WizardShell />
      </WizardProvider>
    </OnboardingApiProvider>
  );
}
