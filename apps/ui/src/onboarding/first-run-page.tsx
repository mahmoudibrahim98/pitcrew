// The first-run wizard at `paths.setup(ws)` (`/w/$ws/onboarding`), where the shell sends a
// workspace that needs setup. A lazy route component (`routes.tsx`), so it is its own chunk.
//
// It runs against the real hub (`hub-api.ts`): Welcome, Workspace, Done. A development build can
// run every step against the fake instead with `?onboarding=fake`; a production build cannot (the
// branch, and the fake with it, is dropped at build time).

import { useSearch } from '@tanstack/react-router';
import { lazy, Suspense, useMemo } from 'react';
import { useRemoteGateway, useSetUp } from '../data/index.ts';
import { OnboardingApiProvider } from './api-context.tsx';
import { createHubOnboardingApi } from './hub-api.ts';
import { WizardProvider } from './wizard-context.tsx';
import { WizardShell } from './wizard-shell.tsx';

const FakeFirstRun = import.meta.env.DEV
  ? lazy(() => import('./fake-first-run.tsx').then((m) => ({ default: m.FakeFirstRun })))
  : null;

export function FirstRunPage() {
  const search: { onboarding?: unknown } = useSearch({ strict: false });
  if (FakeFirstRun !== null && search.onboarding === 'fake') {
    return (
      <Suspense fallback={null}>
        <FakeFirstRun />
      </Suspense>
    );
  }
  return <HubFirstRun />;
}

function HubFirstRun() {
  const setUp = useSetUp();
  const remote = useRemoteGateway();
  const api = useMemo(() => createHubOnboardingApi({ setUp, remote }), [setUp, remote]);
  return (
    <OnboardingApiProvider api={api}>
      <WizardProvider>
        <WizardShell />
      </WizardProvider>
    </OnboardingApiProvider>
  );
}
