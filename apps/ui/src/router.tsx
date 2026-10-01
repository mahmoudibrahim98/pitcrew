// The app's router: the shell composed with the three feature folders.

import { feature as consoleFeature } from './console/index.ts';
import { feature as onboardingFeature } from './onboarding/index.ts';
import { feature as projectsFeature } from './projects/index.ts';
import { createAppRouter } from './shell/routes.tsx';

export const router = createAppRouter([projectsFeature, consoleFeature, onboardingFeature]);

declare module '@tanstack/react-router' {
  interface Register {
    router: typeof router;
  }
}
