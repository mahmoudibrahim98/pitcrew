// `<AppData>`'s browser half, for development: the configured hub over HTTP and WebSockets, with
// the dev token. Loaded on demand, and only by a development build outside the desktop app: the
// desktop webview never loads it, and a production build does not contain it.

import { useState, type ReactNode } from 'react';
import { createApi } from './api.ts';
import { browserConfig } from './config.ts';
import { createQueryClient, DataProvider } from './provider.tsx';
import { browserTransport } from './transport.ts';

export function BrowserData({ children }: { children: ReactNode }) {
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
