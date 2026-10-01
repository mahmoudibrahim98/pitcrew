// The app's data layer, chosen once at start: the desktop gateway when the UI runs in the desktop
// app's webview, HTTP and WebSockets to the configured hub in a browser.

import { useEffect, useState, type ReactNode } from 'react';
import { createApi } from './api.ts';
import { browserConfig } from './config.ts';
import { createQueryClient, DataProvider } from './provider.tsx';
import { browserTransport, isDesktop } from './transport.ts';
import { WorkspacesProvider, type Gateway } from './workspaces.tsx';

export function AppData({ children }: { children: ReactNode }) {
  const [desktop] = useState(isDesktop);
  return desktop ? <DesktopData>{children}</DesktopData> : <BrowserData>{children}</BrowserData>;
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

/** Loads the gateway, and `@tauri-apps/api` with it, only here: never in a browser. */
function DesktopData({ children }: { children: ReactNode }) {
  const [gateway, setGateway] = useState<Gateway>();
  const [failed, setFailed] = useState<string>();
  useEffect(() => {
    let current = true;
    import('./gateway.ts').then(
      (module) => {
        if (current) setGateway(module.createGateway());
      },
      (error: unknown) => {
        if (current) setFailed(error instanceof Error ? error.message : String(error));
      },
    );
    return () => {
      current = false;
    };
  }, []);
  if (gateway === undefined) {
    return failed === undefined ? null : (
      <p role="alert" className="p-6 text-sm">
        PitCrew could not start its connection to the desktop app: {failed}
      </p>
    );
  }
  return <WorkspacesProvider gateway={gateway}>{children}</WorkspacesProvider>;
}
