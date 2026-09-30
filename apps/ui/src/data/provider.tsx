// Gives the tree the API client and the query cache, and keeps the cache live through the stream.

import { QueryClient, QueryClientProvider, useQuery, useQueryClient } from '@tanstack/react-query';
import { createContext, use, useEffect, type ReactNode } from 'react';
import { create } from 'zustand';
import { ApiError, type Api } from './api.ts';
import { keys } from './keys.ts';
import { connectLive } from './live.ts';
import type { StreamStatus } from './stream.ts';

const ApiContext = createContext<Api | null>(null);

export function useApi(): Api {
  const api = use(ApiContext);
  if (api === null) throw new Error('useApi must be used inside <DataProvider>');
  return api;
}

/** The stream's state, for UI that shows whether data is live. */
export const useConnection = create<{ status: StreamStatus }>(() => ({ status: 'stopped' }));

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

type Workspace = Awaited<ReturnType<Api['workspace']>>;

function LiveUpdates({ api, token }: { api: Api; token: string | undefined }): null {
  const queryClient = useQueryClient();
  const ready = useQuery({
    queryKey: keys.workspace,
    queryFn: ({ signal }) => api.workspace(signal),
    select: () => true,
  }).data;

  useEffect(() => {
    if (ready !== true) return;
    // Resume from the revision of the first workspace read, so nothing between that read and
    // the connection is missed. Later refetches of the workspace do not restart the stream.
    const since = queryClient.getQueryData<Workspace>(keys.workspace)?.rev;
    const client = connectLive({
      queryClient,
      baseUrl: api.baseUrl,
      token,
      since,
      onStatus: (status) => useConnection.setState({ status }),
    });
    return () => client.stop();
  }, [ready, api, token, queryClient]);

  return null;
}

export function DataProvider(props: {
  api: Api;
  queryClient: QueryClient;
  token: string | undefined;
  children: ReactNode;
}) {
  return (
    <QueryClientProvider client={props.queryClient}>
      <ApiContext value={props.api}>
        <LiveUpdates api={props.api} token={props.token} />
        {props.children}
      </ApiContext>
    </QueryClientProvider>
  );
}
