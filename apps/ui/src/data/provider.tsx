// Gives the tree the API client and the query cache, and keeps the cache live through the stream.

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
import type { SocketFactory } from './stream.ts';

const ApiContext = createContext<Api | null>(null);
const LiveContext = createContext<Live | null>(null);

export function useApi(): Api {
  const api = use(ApiContext);
  if (api === null) throw new Error('useApi must be used inside <DataProvider>');
  return api;
}

function useLive<T>(select: (state: LiveState) => T): T {
  const live = use(LiveContext);
  if (live === null) throw new Error('useLive must be used inside <DataProvider>');
  return useStore(live.store, select);
}

/** The stream's state, for UI that shows whether data is live. */
export function useConnection(): LiveState {
  const status = useLive((s) => s.status);
  const synced = useLive((s) => s.synced);
  const problem = useLive((s) => s.problem);
  return { status, synced, problem };
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

export function DataProvider(props: {
  api: Api;
  queryClient: QueryClient;
  token: string | undefined;
  /** For tests. */
  socket?: SocketFactory;
  children: ReactNode;
}) {
  const [live] = useState(() =>
    createLive({
      queryClient: props.queryClient,
      baseUrl: props.api.baseUrl,
      token: props.token,
      probe: () => props.api.me(),
      ...(props.socket === undefined ? {} : { socket: props.socket }),
    }),
  );

  useEffect(() => {
    live.start();
    return () => live.stop();
  }, [live]);

  return (
    <QueryClientProvider client={props.queryClient}>
      <ApiContext value={props.api}>
        <LiveContext value={live}>{props.children}</LiveContext>
      </ApiContext>
    </QueryClientProvider>
  );
}
