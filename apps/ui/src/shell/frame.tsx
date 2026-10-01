// The frame every page lives in: sidebar, top bar, the page, and the Orchestrator panel.

import { Outlet, useRouter } from '@tanstack/react-router';
import { lazy, Suspense, useEffect, type MouseEvent } from 'react';
import { useWorkspace } from '../data/index.ts';
import { TooltipProvider } from '../design/index.ts';
import { CreateDialog } from './create.tsx';
import { LayoutMemory, useLayout, useWorkspaceId } from './layout.ts';
import { OrchestratorPanel } from './orchestrator.tsx';
import { NotFoundPage } from './pages/not-found.tsx';
import { useShellShortcuts } from './shortcuts.ts';
import { Sidebar } from './sidebar.tsx';
import { useShell } from './store.ts';
import { TopBar } from './top-bar.tsx';

const loadPalette = () => import('./palette.tsx');
const Palette = lazy(() => loadPalette().then((m) => ({ default: m.Palette })));

/** Fetches the palette's chunk once the shell is idle, so Ctrl K opens at once. */
function usePrefetchPalette(): void {
  useEffect(() => {
    const idle = window.requestIdleCallback ?? ((run: () => void) => window.setTimeout(run, 500));
    const cancel = window.cancelIdleCallback ?? window.clearTimeout;
    const handle = idle(() => void loadPalette());
    return () => cancel(handle);
  }, []);
}

function skipToMain(event: MouseEvent<HTMLAnchorElement>) {
  event.preventDefault();
  document.getElementById('main')?.focus();
}

export function WorkspaceFrame() {
  const router = useRouter();
  const ws = useWorkspaceId();
  const layout = useLayout();
  const known = useWorkspace().data?.workspace;
  const paletteOpen = useShell((s) => s.paletteOpen);
  const orchestratorOpen = useShell((s) => s.orchestratorOpen);
  useShellShortcuts(router, ws);
  usePrefetchPalette();

  if (known !== undefined && known.id !== ws) {
    return (
      <main className="min-h-dvh bg-bg text-ink">
        <NotFoundPage />
      </main>
    );
  }

  return (
    <TooltipProvider>
      <a
        href="#main"
        onClick={skipToMain}
        className="sr-only z-50 rounded-sm bg-card px-3 py-2 text-sm text-ink shadow-pop focus:not-sr-only focus:fixed focus:top-2 focus:left-2"
      >
        Skip to content
      </a>
      <div className="flex h-dvh overflow-hidden bg-bg text-ink" data-layout={layout}>
        <Sidebar />
        <div className="flex min-w-0 flex-1 flex-col">
          <TopBar />
          <main id="main" tabIndex={-1} className="min-h-0 flex-1 overflow-y-auto outline-none">
            <Outlet />
          </main>
        </div>
        {orchestratorOpen && <OrchestratorPanel />}
      </div>
      <LayoutMemory />
      <CreateDialog />
      {paletteOpen && (
        <Suspense fallback={null}>
          <Palette />
        </Suspense>
      )}
    </TooltipProvider>
  );
}
