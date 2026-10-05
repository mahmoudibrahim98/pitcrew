// Board drafts (api-v1.md, "Board drafts"): the wire types and the hooks. An agent drafts a
// workstream's board from its history; the person sees what will be sent and an estimate first,
// confirms per workstream, and reviews the proposal; nothing is created until they accept it.
//
// The data layer's invalidation map refreshes `draftKeys.all` on each `board_*` event. The hooks
// here also refresh their own keys after a write, and poll while a draft is running (its end comes
// as a session event).

import { useMutation, useQueryClient } from '@tanstack/react-query';
import type { BoardDraft } from '../../../../packages/protocol-ts/bindings/BoardDraft.ts';
import type { DraftCost } from '../../../../packages/protocol-ts/bindings/DraftCost.ts';
import type { DraftPreview } from '../../../../packages/protocol-ts/bindings/DraftPreview.ts';
import type { StartDraft } from '../../../../packages/protocol-ts/bindings/StartDraft.ts';
import { useApi, useLiveQuery, type SessionId, type Task, type WorkstreamId } from '../data/index.ts';

// The board types are the protocol's own, generated from `crates/protocol` (`board.rs`).
export type { BoardDraft } from '../../../../packages/protocol-ts/bindings/BoardDraft.ts';
export type { BoardProposal } from '../../../../packages/protocol-ts/bindings/BoardProposal.ts';
export type { DraftCost } from '../../../../packages/protocol-ts/bindings/DraftCost.ts';
export type { DraftPreview } from '../../../../packages/protocol-ts/bindings/DraftPreview.ts';
export type { DraftState } from '../../../../packages/protocol-ts/bindings/DraftState.ts';
export type { DraftedTask } from '../../../../packages/protocol-ts/bindings/DraftedTask.ts';
export type { ProposedTask } from '../../../../packages/protocol-ts/bindings/ProposedTask.ts';
export type { StartDraft } from '../../../../packages/protocol-ts/bindings/StartDraft.ts';
export type { UsageEstimate } from '../../../../packages/protocol-ts/bindings/UsageEstimate.ts';

/**
 * The review's answer: the generated `DraftReviewed`, with its tasks as the UI's `Task` (the data
 * layer's, which the rest of the UI holds; the same wire shape, see `packages/protocol-ts/DRIFT.md`).
 */
export interface DraftReviewed {
  draft: BoardDraft;
  tasks: Task[];
}

/** How often a running draft is looked at again. */
export const RUNNING_POLL_MS = 4000;

export const draftKeys = {
  all: ['board-drafts'] as const,
  list: (workstream: WorkstreamId) => ['board-drafts', 'list', workstream] as const,
  preview: (workstream: WorkstreamId) => ['board-drafts', 'preview', workstream] as const,
};

const id = encodeURIComponent;

/** A workstream's drafts, newest first; polled while one runs. */
export function useBoardDrafts(workstream: WorkstreamId) {
  const api = useApi();
  return useLiveQuery({
    queryKey: draftKeys.list(workstream),
    queryFn: ({ signal }) =>
      api.request<BoardDraft[]>('GET', '/v1/board-drafts', { query: { workstream }, signal }),
    refetchInterval: (query) =>
      query.state.data?.some((d) => d.state === 'running') === true ? RUNNING_POLL_MS : false,
  });
}

/** What a draft of `workstream` would send, and its estimate. Stores nothing on the hub. */
export function useDraftPreview(workstream: WorkstreamId, enabled = true) {
  const api = useApi();
  return useLiveQuery({
    queryKey: draftKeys.preview(workstream),
    queryFn: ({ signal }) =>
      api.request<DraftPreview>('GET', `/v1/workstreams/${id(workstream)}/board-draft`, { signal }),
    enabled,
  });
}

function useRefresh(workstream: WorkstreamId) {
  const queries = useQueryClient();
  return () =>
    Promise.all([
      queries.invalidateQueries({ queryKey: draftKeys.list(workstream) }),
      queries.invalidateQueries({ queryKey: draftKeys.preview(workstream) }),
    ]);
}

/** Starts drafting with the preview the person confirmed (its `digest`). */
export function useStartDraft(workstream: WorkstreamId) {
  const api = useApi();
  const refresh = useRefresh(workstream);
  return useMutation({
    mutationFn: (start: StartDraft) =>
      api.request<BoardDraft>('POST', `/v1/workstreams/${id(workstream)}/board-drafts`, { body: start }),
    onSettled: refresh,
  });
}

/** Accepts the proposal's items at `accept` (indexes); the rest create nothing. */
export function useReviewDraft(workstream: WorkstreamId) {
  const api = useApi();
  const refresh = useRefresh(workstream);
  return useMutation({
    mutationFn: ({ draft, accept }: { draft: string; accept: number[] }) =>
      api.request<DraftReviewed>('POST', `/v1/board-drafts/${id(draft)}/review`, { body: { accept } }),
    onSettled: refresh,
  });
}

/** Ends a running draft's session, which ends the draft. */
export function useStopDraft(workstream: WorkstreamId) {
  const api = useApi();
  const refresh = useRefresh(workstream);
  return useMutation({
    mutationFn: (session: SessionId) =>
      api.request<undefined>('POST', `/v1/sessions/${id(session)}/end`, { body: { mode: 'graceful' } }),
    onSettled: refresh,
  });
}

/** `1234` → `1.2 KB`. */
export function sizeLabel(bytes: number): string {
  if (bytes < 1024) return `${bytes} bytes`;
  return `${(bytes / 1024).toFixed(1)} KB`;
}

/** `18432` → `about 18,000`. */
export function tokensLabel(tokens: number): string {
  const rounded = tokens < 1000 ? tokens : Math.round(tokens / 1000) * 1000;
  return rounded.toLocaleString('en-US');
}

/** One line for what a draft sends: sessions, tasks, sizes, redactions. */
export function costLine(cost: DraftCost): string {
  const sessions = `${cost.sessions} session${cost.sessions === 1 ? '' : 's'}${
    cost.sessions_left_out > 0 ? ` (${cost.sessions_left_out} older left out)` : ''
  }`;
  const tasks = `${cost.tasks} existing task${cost.tasks === 1 ? '' : 's'}`;
  const redacted =
    cost.redacted === 0 ? 'nothing redacted' : `${cost.redacted} secret-looking item${cost.redacted === 1 ? '' : 's'} redacted`;
  return `${sessions}, ${tasks}: a ${sizeLabel(cost.summary_bytes)} summary in a ${sizeLabel(cost.prompt_bytes)} prompt; ${redacted}.`;
}

/** The estimate, in words. */
export function estimateLine(cost: DraftCost): string {
  return `Estimated usage: about ${tokensLabel(cost.estimate.input_tokens)} tokens read, at most ${tokensLabel(
    cost.estimate.output_tokens,
  )} written for the proposal, on your own agent’s plan.`;
}
