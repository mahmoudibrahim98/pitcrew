// The onboarding feature's routes:
// - `/w/$ws/onboarding` (`paths.setup`): the first-run wizard, where the shell sends a workspace
//   that needs setup. `staticData.setup` exempts it from that redirect and shows it bare.
// - `/connect` (`paths.connect()`, a root route): connect a remote machine, in the desktop app.
//   Outside any workspace, since it also runs when there is none yet.
// Both lazy: each wizard is its own chunk.

import { createRoute, lazyRouteComponent } from '@tanstack/react-router';
import type { ShellRootRoute, WorkspaceRoute } from '../shell/index.ts';

export function onboardingRoutes(parent: WorkspaceRoute) {
  return [
    createRoute({
      getParentRoute: () => parent,
      path: 'onboarding',
      staticData: { title: 'Welcome', setup: true },
      component: lazyRouteComponent(() => import('./first-run-page.tsx'), 'FirstRunPage'),
    }),
  ];
}

export function onboardingRootRoutes(root: ShellRootRoute) {
  return [
    createRoute({
      getParentRoute: () => root,
      path: 'connect',
      component: lazyRouteComponent(() => import('./connect/connect-page.tsx'), 'ConnectPage'),
    }),
  ];
}
