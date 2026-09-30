// Query keys. Lists and details sit under separate prefixes so an event can invalidate every list
// of a kind (`['tasks', 'list']`) without touching unrelated details, and one detail by id.

import type { AskFilters, SessionFilters, TaskFilters } from './types.ts';

export const keys = {
  me: ['me'] as const,
  workspace: ['workspace'] as const,
  machines: ['machines'] as const,
  members: ['members'] as const,
  events: ['events'] as const,
  briefs: ['briefs'] as const,
  dispatches: ['dispatches'] as const,

  projects: {
    lists: ['projects', 'list'] as const,
    list: () => ['projects', 'list'] as const,
    detail: (id: string) => ['projects', 'detail', id] as const,
  },
  workstreams: {
    lists: ['workstreams', 'list'] as const,
    list: (project?: string) => ['workstreams', 'list', { project }] as const,
    detail: (id: string) => ['workstreams', 'detail', id] as const,
  },
  tasks: {
    lists: ['tasks', 'list'] as const,
    list: (filters: TaskFilters = {}) => ['tasks', 'list', filters] as const,
    detail: (id: string) => ['tasks', 'detail', id] as const,
  },
  sessions: {
    lists: ['sessions', 'list'] as const,
    list: (filters: SessionFilters = {}) => ['sessions', 'list', filters] as const,
    detail: (id: string) => ['sessions', 'detail', id] as const,
    transcript: (id: string) => ['sessions', 'transcript', id] as const,
  },
  asks: {
    lists: ['asks', 'list'] as const,
    list: (filters: AskFilters = {}) => ['asks', 'list', filters] as const,
  },
};
