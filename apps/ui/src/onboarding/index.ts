// Onboarding (stream O): the first-run wizard, and connecting a remote machine in the desktop app.
// Routes only, both lazy; the shell's switcher and its "No workspaces yet" screen open the connect
// wizard, and the shell sends a workspace that needs setup to the first-run wizard. See README.md.

import { defineFeature } from '../shell/index.ts';
import { onboardingRootRoutes, onboardingRoutes } from './routes.tsx';

export const feature = defineFeature({
  id: 'onboarding',
  layout: 'both',
  routes: onboardingRoutes,
  rootRoutes: onboardingRootRoutes,
});
