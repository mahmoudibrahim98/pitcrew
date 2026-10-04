// Outward writes to GitHub and Jira (api-v1.md, "Outward writes: every one approved first"): a
// client over the routes, and the hooks the Inbox's approval cards and the task drawer use.
//
// Nothing here sends anything upstream. Approving is answering the write's approval ask with
// "Send" (`POST /v1/asks/{id}/answer`, option 0); the hub then sends exactly `proposal.after`.

import { useMutation, useQueryClient } from '@tanstack/react-query';
import {
  keys,
  useApi,
  useLiveQuery,
  type Api,
  type Ask,
  type TaskId,
  type UpstreamWrite,
  type WriteFields,
} from '../../data/index.ts';

/** The option of an approval ask that sends, and the one that does not. */
export const SEND = 0;
export const DONT_SEND = 1;

const enc = encodeURIComponent;

/** The writes routes over `api`. */
export function writeClient(api: Api) {
  return {
    ofTask: (task: TaskId, signal?: AbortSignal) =>
      api.request<UpstreamWrite[]>('GET', '/v1/writes', { query: { task }, signal }),
    one: (ask: string, signal?: AbortSignal) => api.request<UpstreamWrite>('GET', `/v1/writes/${enc(ask)}`, { signal }),
    request: (task: TaskId, operation: 'create_issue' | 'comment', text?: string) =>
      api.request<UpstreamWrite>('POST', '/v1/writes', {
        body: text === undefined ? { task, operation } : { task, operation, text },
      }),
    retry: (ask: string) => api.request<UpstreamWrite>('POST', `/v1/writes/${enc(ask)}/retry`),
  };
}

/** A task's writes, oldest first. Kept current by the write events. */
export function useTaskWrites(task: TaskId) {
  const api = useApi();
  return useLiveQuery({
    queryKey: keys.writes.task(task),
    queryFn: ({ signal }) => writeClient(api).ofTask(task, signal),
  });
}

/**
 * The write an approval ask proposes, if it proposes one: an approval ask the hub raised has one;
 * any other has none (`404`), and then the card shows the ask as it is.
 */
export function useWriteOf(ask: Ask) {
  const api = useApi();
  return useLiveQuery({
    queryKey: keys.writes.one(ask.id),
    queryFn: ({ signal }) => writeClient(api).one(ask.id, signal),
    enabled: ask.kind === 'approval',
    retry: false,
  });
}

/** Asking for a write, and retrying a failed one. Each refreshes every write list. */
export function useWriteActions() {
  const api = useApi();
  const queryClient = useQueryClient();
  const client = writeClient(api);
  const refresh = () => queryClient.invalidateQueries({ queryKey: keys.writes.all });
  return {
    request: useMutation({
      mutationFn: ({ task, operation, text }: { task: TaskId; operation: 'create_issue' | 'comment'; text?: string }) =>
        client.request(task, operation, text),
      onSettled: refresh,
    }),
    retry: useMutation({ mutationFn: (ask: string) => client.retry(ask), onSettled: refresh }),
  };
}

/** One row of a write's diff: a field, what upstream had, and exactly what is sent. */
export interface FieldRow {
  field: string;
  before?: string;
  after: string;
}

const FIELD_ORDER = ['title', 'body', 'labels', 'milestone', 'epic', 'state', 'comment'] as const;

function shown(fields: WriteFields, field: (typeof FIELD_ORDER)[number]): string | undefined {
  switch (field) {
    case 'labels':
      return fields.labels === undefined ? undefined : fields.labels.length === 0 ? '(none)' : fields.labels.join(', ');
    case 'state': {
      if (fields.state === undefined) return undefined;
      const reason =
        fields.close_reason === 'completed' ? ' (completed)' : fields.close_reason === 'not_planned' ? ' (not planned)' : '';
      return `${fields.state}${reason}`;
    }
    default:
      return fields[field];
  }
}

/** The rows of a write's diff, one per field it sends, in a fixed order. */
export function fieldRows(write: UpstreamWrite): FieldRow[] {
  const { before, after, system } = write.proposal;
  const rows: FieldRow[] = [];
  for (const field of FIELD_ORDER) {
    const a = shown(after, field);
    if (a === undefined) continue;
    const b = shown(before, field);
    const name = field === 'body' && system === 'jira' ? 'description' : field === 'title' && system === 'jira' ? 'summary' : field;
    rows.push(b === undefined ? { field: name, after: a } : { field: name, before: b, after: a });
  }
  return rows;
}
