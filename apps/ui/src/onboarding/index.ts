// The first-run and add-a-machine wizards (stream O). Routes and the palette command; see
// README.md for the proposed `OnboardingApi` contract the wizards are built against.

import { defineFeature } from '../shell/index.ts';
import { onboardingRoutes } from './routes.tsx';

export const feature = defineFeature({
  id: 'onboarding',
  layout: 'both',
  routes: onboardingRoutes,
  commands: [
    {
      id: 'add-machine',
      label: 'Add a machine',
      group: 'Workspace',
      keywords: ['ssh', 'wsl', 'connect', 'machine', 'setup'],
      run: (c) => c.go('onboarding/add-machine'),
    },
  ],
});
