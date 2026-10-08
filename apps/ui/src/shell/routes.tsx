// Builds the router: `/` opens the workspace, `/w/$ws` is the shell frame, and under it the
// features' routes plus the shell's placeholders for the paths no feature serves yet.

import {
  createRootRoute,
  createRoute,
  createRouter,
  lazyRouteComponent,
  Outlet,
  type AnyRoute,
  type RouterHistory,
} from '@tanstack/react-router';
import { useApplyTheme } from '../design/index.ts';
import { RegistryContext } from './context.ts';
import { shellFeature } from './core.tsx';
import type { Feature } from './feature.ts';
import { WorkspaceFrame } from './frame.tsx';
import { useGatewayNavigation } from './gateway-navigate.ts';
import { lazy, Suspense } from 'react';
import { isDesktop } from '../data/transport.ts';
import { Notice } from './notice.tsx';
import { LoadingSplash } from './loading.tsx';
import { NotFoundPage, RootNotFound } from './pages/not-found.tsx';
import { OpenLayout, OpenWorkspace } from './pages/open.tsx';
import { GatewayPrompts } from './prompts.tsx';
import { composeFeatures, servedPaths, withLayout } from './registry.ts';

const DesktopUpdates = lazy(() => import('./updates.tsx').then((m) => ({ default: m.DesktopUpdates })));

function Root() {
  useApplyTheme();
  useGatewayNavigation();
  const content = (
    <>
      <Notice />
      <Outlet />
      {/* SSH's questions, in the desktop app, whatever is on screen: mounted once, here. */}
      <GatewayPrompts />
    </>
  );
  return isDesktop() ? <Suspense fallback={<LoadingSplash />}><DesktopUpdates>{content}</DesktopUpdates></Suspense> : content;
}

function createShellRoot() {
  return createRootRoute({ component: Root, notFoundComponent: RootNotFound });
}

/** The parent of every feature's `rootRoutes`: `/`. */
export type ShellRootRoute = ReturnType<typeof createShellRoot>;

export function createWorkspaceRoute(root: ReturnType<typeof createShellRoot>) {
  return createRoute({
    getParentRoute: () => root,
    path: 'w/$ws',
    component: WorkspaceFrame,
    notFoundComponent: NotFoundPage,
  });
}

/** The parent of every feature route: `/w/$ws`. */
export type WorkspaceRoute = ReturnType<typeof createWorkspaceRoute>;

export function createAppRouter(features: readonly Feature[], options: { history?: RouterHistory } = {}) {
  const registry = composeFeatures(shellFeature, features);
  const root = createShellRoot();
  const workspace = createWorkspaceRoute(root);

  const featureRoutes = features.flatMap((f) => withLayout(f.routes?.(workspace) ?? [], f.layout));
  const served = new Set(featureRoutes.flatMap((route) => servedPaths(route)));
  const placeholders = (shellFeature.routes?.(workspace) ?? []).filter(
    (route) => !servedPaths(route).some((path) => served.has(path)),
  );

  const children: AnyRoute[] = [
    createRoute({ getParentRoute: () => root, path: '/', component: OpenWorkspace }),
    workspace.addChildren([
      createRoute({ getParentRoute: () => workspace, path: '/', component: OpenLayout }),
      ...placeholders,
      ...featureRoutes,
    ]),
    ...features.flatMap((f) => f.rootRoutes?.(root) ?? []),
  ];
  if (import.meta.env.DEV) {
    children.push(
      createRoute({
        getParentRoute: () => root,
        path: 'dev/proof',
        component: lazyRouteComponent(() => import('./proof-page.tsx'), 'ProofPage'),
      }),
    );
  }

  return createRouter({
    routeTree: root.addChildren(children),
    ...(options.history === undefined ? {} : { history: options.history }),
    defaultPreload: 'intent',
    defaultPendingComponent: LoadingSplash,
    Wrap: ({ children: app }) => <RegistryContext value={registry}>{app}</RegistryContext>,
  });
}

export type AppRouter = ReturnType<typeof createAppRouter>;
