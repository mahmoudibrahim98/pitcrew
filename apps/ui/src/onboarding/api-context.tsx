// Provides the `OnboardingApi` to the wizard tree. The routes wire up the fake (`fake-api.ts`);
// tests pass their own instance (usually `createFakeOnboardingApi({ speed: 0 })`) so they do not
// wait on real timers.

import { createContext, use, useState, type ReactNode } from 'react';
import { createFakeOnboardingApi } from './fake-api.ts';
import type { OnboardingApi } from './api.ts';

const OnboardingApiContext = createContext<OnboardingApi | null>(null);

export function useOnboardingApi(): OnboardingApi {
  const api = use(OnboardingApiContext);
  if (api === null) throw new Error('useOnboardingApi must be used inside <OnboardingApiProvider>');
  return api;
}

export function OnboardingApiProvider({
  api,
  children,
}: {
  /** Defaults to a fresh fake. Pass one in tests to control its speed or seed its state. */
  api?: OnboardingApi;
  children: ReactNode;
}) {
  const [fallback] = useState(() => api ?? createFakeOnboardingApi());
  return <OnboardingApiContext value={api ?? fallback}>{children}</OnboardingApiContext>;
}
