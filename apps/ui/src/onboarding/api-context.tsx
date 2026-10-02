// Provides the `OnboardingApi` to the wizard tree. The first-run page gives the real one
// (`hub-api.ts`), or the fake in a development build that asks for it; tests pass their own
// (usually `createFakeOnboardingApi({ speed: 0 })`) so they do not wait on real timers. There is no
// default: nothing falls back to the fake.

import { createContext, use, type ReactNode } from 'react';
import type { OnboardingApi } from './api.ts';

const OnboardingApiContext = createContext<OnboardingApi | null>(null);

export function useOnboardingApi(): OnboardingApi {
  const api = use(OnboardingApiContext);
  if (api === null) throw new Error('useOnboardingApi must be used inside <OnboardingApiProvider>');
  return api;
}

export function OnboardingApiProvider({ api, children }: { api: OnboardingApi; children: ReactNode }) {
  return <OnboardingApiContext value={api}>{children}</OnboardingApiContext>;
}
