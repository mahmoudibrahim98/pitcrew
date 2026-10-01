// The frame every page lives in: sidebar, top bar, the page, and the Orchestrator panel. It sits
// inside the workspace's data scope (in the desktop app, each workspace has its own).

import { Outlet, useRouter } from '@tanstack/react-router';
import { lazy, Suspense, useEffect, type MouseEvent } from 'react';
import { useGatewayWorkspace, useWorkspace, WorkspaceScope, type ScopeFallback } from '../data/index.ts';
import { TooltipProvider } from '../design/index.ts';
import { CreateDialog } from './create.tsx';
import { LayoutMemory, useLayout, useWorkspaceId } from './layout.ts';
import { OrchestratorPanel } from './orchestrator.tsx';
import { NotFoundPage } from './pages/not-found.tsx';
import { StatusScreen, WorkspaceUnavailable } from './pages/unavailable.tsx';
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

function ScopeMissing({ reason }: { reason: ScopeFallback }) {
  if (reason.kind === 'loading') return <StatusScreen>Loading workspaces…</StatusScreen>;
  if (reason.kind === 'failed') return <StatusScreen>Could not list the workspaces: {reason.message}</StatusScreen>;
  return (
    <main className="min-h-dvh bg-bg text-ink">
      <NotFoundPage />
    </main>
  );
}

/** `/w/$ws`: the frame, in the workspace's data scope. Another workspace remounts everything. */
export function WorkspaceFrame() {
  const ws = useWorkspaceId();
  return (
    <WorkspaceScope key={ws} ws={ws} fallback={(reason) => <ScopeMissing reason={reason} />}>
      <Frame />
    </WorkspaceScope>
  );
}

function Frame() {
  const router = useRouter();
  const ws = useWorkspaceId();
  const layout = useLayout();
  const known = useWorkspace().data?.workspace;
  // The gateway's entry, in the desktop app: a workspace it cannot reach shows why, not a page.
  const gateway = useGatewayWorkspace();
  const unavailable = gateway?.state === 'unreachable' || gateway?.state === 'needs_pairing' ? gateway : undefined;
  const paletteOpen = useShell((s) => s.paletteOpen);
  const orchestratorOpen = useShell((s) => s.orchestratorOpen);
  const setLastWorkspace = useShell((s) => s.setLastWorkspace);
  useShellShortcuts(router, ws);
  usePrefetchPalette();
  useEffect(() => setLastWorkspace(ws), [setLastWorkspace, ws]);

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
            {unavailable === undefined ? <Outlet /> : <WorkspaceUnavailable workspace={unavailable} />}
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
