// Shared test harness (not a test file itself: no `.test.` in the name, so Vitest's `include`
// glob skips it). `renderWizard` mounts the real `WizardShell` behind a minimal router, so
// `useWorkspaceId()` and `useRouter()` (used by the sign-in and done steps) work without needing
// the shell's full frame or a mock hub.

import { render } from '@testing-library/react';
import { createMemoryHistory, createRootRoute, createRoute, createRouter, RouterProvider } from '@tanstack/react-router';
import { OnboardingApiProvider } from './api-context.tsx';
import type { OnboardingApi } from './api.ts';
import { createFakeOnboardingApi } from './fake-api.ts';
import { WizardProvider } from './wizard-context.tsx';
import { WizardShell } from './wizard-shell.tsx';
import type { WizardMode } from './wizard-state.ts';

export const TEST_WS = 'ws-test';

export function renderWizard(mode: WizardMode, api?: OnboardingApi) {
  const theApi = api ?? createFakeOnboardingApi({ speed: 0 });
  const root = createRootRoute();
  const wizardRoute = createRoute({
    getParentRoute: () => root,
    path: 'w/$ws/wizard',
    component: () => (
      <OnboardingApiProvider api={theApi}>
        <WizardProvider mode={mode}>
          <WizardShell />
        </WizardProvider>
      </OnboardingApiProvider>
    ),
  });
  const homeRoute = createRoute({
    getParentRoute: () => root,
    path: 'w/$ws/home',
    component: () => <h1>Home</h1>,
  });
  const router = createRouter({
    routeTree: root.addChildren([wizardRoute, homeRoute]),
    history: createMemoryHistory({ initialEntries: [`/w/${TEST_WS}/wizard`] }),
  });
  return { ...render(<RouterProvider router={router} />), router, api: theApi };
}
