// The app's data layer, chosen once at start:
// - in the desktop app's webview (`window.__TAURI_INTERNALS__`), the gateway (`desktop.tsx`);
// - in a browser, during development only, the configured hub (`browser.tsx`);
// - otherwise (a production build in a browser) nothing: it fails closed, and never reaches a
//   daemon itself.
// Each half is loaded on demand: the desktop webview never loads the browser's config or token.

import { useEffect, useState, type ComponentType, type ReactNode } from 'react';
import { isDesktop } from './transport.ts';

type Root = ComponentType<{ children: ReactNode }>;
type Load = () => Promise<Root>;

const loadDesktop: Load = () => import('./desktop.tsx').then((module) => module.DesktopData);

/**
 * The browser's data layer, in development only. The literal `import.meta.env.DEV` lets the
 * bundler drop it, the browser's config and its dev token from production builds.
 */
function browserLoader(): Load | undefined {
  return import.meta.env.DEV ? () => import('./browser.tsx').then((module) => module.BrowserData) : undefined;
}

export function AppData({ children }: { children: ReactNode }) {
  const [load] = useState(() => (isDesktop() ? loadDesktop : browserLoader()));
  if (load === undefined) return <NotInDesktop />;
  return <Loaded load={load}>{children}</Loaded>;
}

function NotInDesktop() {
  return (
    <main role="alert" className="grid min-h-dvh place-items-center bg-bg px-6 text-sm text-ink">
      PitCrew runs in its desktop app. This build does not connect to a daemon from a browser.
    </main>
  );
}

function Loaded({ load, children }: { load: Load; children: ReactNode }) {
  const [root, setRoot] = useState<{ Root: Root }>();
  const [failed, setFailed] = useState<string>();
  useEffect(() => {
    let current = true;
    load().then(
      (Root) => {
        if (current) setRoot({ Root });
      },
      (error: unknown) => {
        if (current) setFailed(error instanceof Error ? error.message : String(error));
      },
    );
    return () => {
      current = false;
    };
  }, [load]);
  if (root === undefined) {
    return failed === undefined ? null : (
      <p role="alert" className="p-6 text-sm">
        PitCrew could not start its data layer: {failed}
      </p>
    );
  }
  return <root.Root>{children}</root.Root>;
}
