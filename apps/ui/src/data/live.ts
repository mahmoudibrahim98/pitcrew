// Connects the delta stream to the TanStack Query cache: each event invalidates the keys it
// touches, and a reset drops the cache.

import type { QueryClient } from '@tanstack/react-query';
import { keysToInvalidate, type CacheLookup } from './invalidation.ts';
import { keys } from './keys.ts';
import { StreamClient, type SocketFactory, type StreamStatus } from './stream.ts';
import type { Task } from './types.ts';

export function cacheLookup(queryClient: QueryClient): CacheLookup {
  return {
    taskWorkstream(id) {
      const detail = queryClient.getQueryData<Task>(keys.tasks.detail(id));
      if (detail !== undefined) return detail.workstream;
      for (const [, tasks] of queryClient.getQueriesData<Task[]>({ queryKey: keys.tasks.lists })) {
        const found = tasks?.find((task) => task.id === id);
        if (found !== undefined) return found.workstream;
      }
      return undefined;
    },
  };
}

export interface LiveOptions {
  queryClient: QueryClient;
  baseUrl: string;
  token?: string | undefined;
  /** The revision the cache is at, from `GET /v1/workspace`. */
  since?: number | undefined;
  onStatus?: (status: StreamStatus) => void;
  socket?: SocketFactory;
}

/** Starts the stream; call `stop()` on the result to end it. */
export function connectLive(options: LiveOptions): StreamClient {
  const { queryClient } = options;
  const cache = cacheLookup(queryClient);
  const client = new StreamClient({
    baseUrl: options.baseUrl,
    token: options.token,
    since: options.since,
    ...(options.socket === undefined ? {} : { socket: options.socket }),
    ...(options.onStatus === undefined ? {} : { onStatus: options.onStatus }),
    onEvents(events) {
      for (const queryKey of keysToInvalidate(events, cache)) {
        void queryClient.invalidateQueries({ queryKey });
      }
    },
    onReset() {
      void queryClient.resetQueries();
    },
  });
  client.start();
  return client;
}
