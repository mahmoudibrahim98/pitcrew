// Query hooks for the shared data. Features add their own hooks in their folders, using
// `useApi()` and `keys` so the stream's invalidation reaches them.

import { useMutation, useQuery } from '@tanstack/react-query';
import { keys } from './keys.ts';
import { useApi } from './provider.tsx';
import type { AskFilters, SessionFilters, TaskFilters, TaskStatus } from './types.ts';

export function useMe() {
  const api = useApi();
  return useQuery({ queryKey: keys.me, queryFn: ({ signal }) => api.me(signal) });
}

export function useWorkspace() {
  const api = useApi();
  return useQuery({ queryKey: keys.workspace, queryFn: ({ signal }) => api.workspace(signal) });
}

export function useMembers() {
  const api = useApi();
  return useQuery({ queryKey: keys.members, queryFn: ({ signal }) => api.members(signal) });
}

export function useProjects() {
  const api = useApi();
  return useQuery({ queryKey: keys.projects.list(), queryFn: ({ signal }) => api.projects(signal) });
}

export function useProject(id: string) {
  const api = useApi();
  return useQuery({
    queryKey: keys.projects.detail(id),
    queryFn: ({ signal }) => api.project(id, signal),
  });
}

export function useWorkstreams(project?: string) {
  const api = useApi();
  return useQuery({
    queryKey: keys.workstreams.list(project),
    queryFn: ({ signal }) => api.workstreams(project, signal),
  });
}

export function useWorkstream(id: string) {
  const api = useApi();
  return useQuery({
    queryKey: keys.workstreams.detail(id),
    queryFn: ({ signal }) => api.workstream(id, signal),
  });
}

export function useTasks(filters: TaskFilters = {}) {
  const api = useApi();
  return useQuery({
    queryKey: keys.tasks.list(filters),
    queryFn: ({ signal }) => api.tasks(filters, signal),
  });
}

/** By id. Keys resolve through `useTasks` or the API directly. */
export function useTask(id: string) {
  const api = useApi();
  return useQuery({ queryKey: keys.tasks.detail(id), queryFn: ({ signal }) => api.task(id, signal) });
}

export function useSessions(filters: SessionFilters = {}) {
  const api = useApi();
  return useQuery({
    queryKey: keys.sessions.list(filters),
    queryFn: ({ signal }) => api.sessions(filters, signal),
  });
}

export function useSession(id: string) {
  const api = useApi();
  return useQuery({
    queryKey: keys.sessions.detail(id),
    queryFn: ({ signal }) => api.session(id, signal),
  });
}

export function useAsks(filters: AskFilters = {}) {
  const api = useApi();
  return useQuery({
    queryKey: keys.asks.list(filters),
    queryFn: ({ signal }) => api.asks(filters, signal),
  });
}

/** Moves a task. The cache is not touched here: the `task_moved` event updates it. */
export function useMoveTask() {
  const api = useApi();
  return useMutation({
    mutationFn: ({ task, to }: { task: string; to: TaskStatus }) => api.moveTask(task, to),
  });
}
