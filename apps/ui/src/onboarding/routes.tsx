// The onboarding feature's routes under `/w/$ws`. See README.md for why the first-run wizard is
// not also reachable at a pre-workspace `/onboarding`.

import { createRoute, lazyRouteComponent } from '@tanstack/react-router';
import type { WorkspaceRoute } from '../shell/index.ts';

export function onboardingRoutes(parent: WorkspaceRoute) {
  return [
    createRoute({
      getParentRoute: () => parent,
      path: 'onboarding',
      staticData: { title: 'Welcome' },
      component: lazyRouteComponent(() => import('./first-run-page.tsx'), 'FirstRunPage'),
    }),
    createRoute({
      getParentRoute: () => parent,
      path: 'onboarding/add-machine',
      staticData: { title: 'Add a machine' },
      component: lazyRouteComponent(() => import('./add-machine-page.tsx'), 'AddMachinePage'),
    }),
  ];
}
