import { useMutation, useQueryClient } from '@tanstack/react-query';
import { useApi, useLiveQuery } from './provider.tsx';

export interface ReadCursor { scope: string; rev: number }

export function useReadCursors() {
  const api = useApi();
  return useLiveQuery<ReadCursor[]>({
    queryKey: ['cursors'],
    queryFn: ({ signal }) => api.request<ReadCursor[]>('GET', '/v1/me/cursors', { signal }),
  });
}

export function useMoveCursor() {
  const api = useApi();
  const client = useQueryClient();
  return useMutation({
    mutationFn: ({ scope, rev, signal }: ReadCursor & { signal?: AbortSignal }) => api.request<ReadCursor>('PUT', `/v1/me/cursors/${encodeURIComponent(scope)}`, { body: { rev }, ...(signal === undefined ? {} : { signal }) }),
    onSuccess: () => client.invalidateQueries({ queryKey: ['cursors'] }),
  });
}
