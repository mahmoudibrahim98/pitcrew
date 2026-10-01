// Gives the tree the API client and the query cache, and keeps the cache live through the stream.
// In a browser `<DataProvider>` does it for the one hub; in the desktop app every workspace has a
// scope of its own (`workspaces.tsx`), with its own cache.

import {
  QueryClient,
  QueryClientProvider,
  useQuery,
  type QueryKey,
  type UseQueryOptions,
} from '@tanstack/react-query';
import { createContext, use, useEffect, useState, type ReactNode } from 'react';
import { useStore } from 'zustand';
import { ApiError, type Api } from './api.ts';
import { createLive, type Live, type LiveState } from './live.ts';
import type { BrowserTransport, SocketFactory, Transport, TransportSocket } from './transport.ts';
import type { GatewayWorkspace } from './workspaces.tsx';

const ApiContext = createContext<Api | null>(null);
const LiveContext = createContext<Live | null>(null);
const GatewayWorkspaceContext = createContext<GatewayWorkspace | null>(null);

export function useApi(): Api {
  const api = use(ApiContext);
  if (api === null) throw new Error('useApi must be used inside <DataProvider>');
  return api;
}

function useLiveContext(): Live {
  const live = use(LiveContext);
  if (live === null) throw new Error('useLive must be used inside <DataProvider>');
  return live;
}

function useLive<T>(select: (state: LiveState) => T): T {
  const live = useLiveContext();
  return useStore(live.store, select);
}

/** The stream's state, for UI that shows whether data is live. */
export function useConnection(): LiveState {
  const status = useLive((s) => s.status);
  const synced = useLive((s) => s.synced);
  const problem = useLive((s) => s.problem);
  return { status, synced, problem };
}

/** In the desktop app, the gateway's entry for this workspace (name, state); none in a browser. */
export function useGatewayWorkspace(): GatewayWorkspace | undefined {
  return use(GatewayWorkspaceContext) ?? undefined;
}

/**
 * Opens a socket on one of API v1's WebSocket routes through the stream's transport: a browser
 * WebSocket in development, the desktop gateway in the app. For the console's terminal:
 * `const open = useOpenSocket(); const socket = open(terminalPath(id, { cols, rows, from }));`
 */
export function useOpenSocket(): (path: string) => TransportSocket {
  const live = useLiveContext();
  return live.transport.openSocket;
}

/**
 * `useQuery` for server state. It stays idle until the stream is synced, so no fetch can miss an
 * event. Every data hook, including the features' own, goes through this.
 */
export function useLiveQuery<TData, TKey extends QueryKey = QueryKey>(
  options: UseQueryOptions<TData, Error, TData, TKey>,
) {
  const synced = useLive((s) => s.synced);
  const { enabled } = options;
  return useQuery({
    ...options,
    enabled:
      typeof enabled === 'function'
        ? (query) => synced && enabled(query)
        : synced && enabled !== false,
  });
}

const NO_RETRY = new Set(['unauthorized', 'forbidden', 'not_found', 'conflict', 'invalid']);

/** The stream keeps queries fresh, so they never go stale on their own. */
export function createQueryClient(): QueryClient {
  return new QueryClient({
    defaultOptions: {
      queries: {
        staleTime: Infinity,
        refetchOnWindowFocus: false,
        retry: (count, error) => !(error instanceof ApiError && NO_RETRY.has(error.code)) && count < 2,
      },
      mutations: { retry: false },
    },
  });
}

/** One hub's or workspace's data, for the tree under it. */
export function DataScope(props: {
  api: Api;
  queryClient: QueryClient;
  live: Live;
  /** The gateway's entry, in the desktop app. */
  workspace?: GatewayWorkspace | undefined;
  children: ReactNode;
}) {
  return (
    <QueryClientProvider client={props.queryClient}>
      <ApiContext value={props.api}>
        <LiveContext value={props.live}>
          <GatewayWorkspaceContext value={props.workspace ?? null}>{props.children}</GatewayWorkspaceContext>
        </LiveContext>
      </ApiContext>
    </QueryClientProvider>
  );
}

function isBrowserTransport(transport: Transport): transport is BrowserTransport {
  return transport.kind === 'browser' && 'with' in transport;
}

/** The stream's transport: the API's, unless a test gives the stream its own token or sockets. */
function streamTransport(transport: Transport, token: string | undefined, socket: SocketFactory | undefined): Transport {
  if (!isBrowserTransport(transport) || (token === undefined && socket === undefined)) return transport;
  return transport.with({ ...(token === undefined ? {} : { token }), ...(socket === undefined ? {} : { socket }) });
}

/** The browser's data layer: one hub, one cache, one stream. */
export function DataProvider(props: {
  api: Api;
  queryClient: QueryClient;
  /**
   * Browser only: the stream's token when it is not the API's (a test acting as an agent, whose
   * token cannot open the stream). Never given in the desktop app, whose gateway adds tokens.
   */
  token?: string | undefined;
  /** For tests: the stream's WebSockets. */
  socket?: SocketFactory;
  children: ReactNode;
}) {
  const [live] = useState(() =>
    createLive({
      queryClient: props.queryClient,
      transport: streamTransport(props.api.transport, props.token, props.socket),
      probe: () => props.api.me(),
    }),
  );

  useEffect(() => {
    live.start();
    return () => live.stop();
  }, [live]);

  return (
    <DataScope api={props.api} queryClient={props.queryClient} live={live}>
      {props.children}
    </DataScope>
  );
}
