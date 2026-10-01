// The app's data layer, chosen once at start: the desktop gateway when the UI runs in the desktop
// app's webview, HTTP and WebSockets to the configured hub in a browser.

import { useEffect, useState, type ComponentType, type ReactNode } from 'react';
import { createApi } from './api.ts';
import { browserConfig } from './config.ts';
import { createQueryClient, DataProvider } from './provider.tsx';
import { browserTransport, isDesktop } from './transport.ts';

export function AppData({ children }: { children: ReactNode }) {
  const [desktop] = useState(isDesktop);
  return desktop ? <LoadDesktop>{children}</LoadDesktop> : <BrowserData>{children}</BrowserData>;
}

function BrowserData({ children }: { children: ReactNode }) {
  const [data] = useState(() => {
    const { baseUrl, token } = browserConfig();
    return { api: createApi({ transport: browserTransport({ baseUrl, token }) }), queryClient: createQueryClient() };
  });
  return (
    <DataProvider api={data.api} queryClient={data.queryClient}>
      {children}
    </DataProvider>
  );
}

type Root = ComponentType<{ children: ReactNode }>;

/** Loads the desktop data layer, with the gateway and `@tauri-apps/api`: never in a browser. */
function LoadDesktop({ children }: { children: ReactNode }) {
  const [Desktop, setDesktop] = useState<{ root: Root }>();
  const [failed, setFailed] = useState<string>();
  useEffect(() => {
    let current = true;
    import('./desktop.tsx').then(
      (module) => {
        if (current) setDesktop({ root: module.DesktopData });
      },
      (error: unknown) => {
        if (current) setFailed(error instanceof Error ? error.message : String(error));
      },
    );
    return () => {
      current = false;
    };
  }, []);
  if (Desktop === undefined) {
    return failed === undefined ? null : (
      <p role="alert" className="p-6 text-sm">
        PitCrew could not start its connection to the desktop app: {failed}
      </p>
    );
  }
  return <Desktop.root>{children}</Desktop.root>;
}
