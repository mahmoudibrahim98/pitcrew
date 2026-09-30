import { createRootRoute, createRoute, createRouter, Outlet } from '@tanstack/react-router';
import { useApplyTheme } from './design/index.ts';
import { ProofPage } from './shell/proof-page.tsx';

function Root() {
  useApplyTheme();
  return (
    <main className="min-h-dvh bg-bg text-ink">
      <Outlet />
    </main>
  );
}

const rootRoute = createRootRoute({ component: Root });

const indexRoute = createRoute({
  getParentRoute: () => rootRoute,
  path: '/',
  component: ProofPage,
});

export const router = createRouter({ routeTree: rootRoute.addChildren([indexRoute]) });

declare module '@tanstack/react-router' {
  interface Register {
    router: typeof router;
  }
}
