// The Orchestrator panel's conversation (API v1, "Orchestrator"): the wire types and the hooks.
// A question runs as a session of an agent CLI the person already uses, which reads with a token
// that may only read; its answer is followed from the session's transcript. The conversations are
// the person's own and are not events, so the stream never carries them: the query polls while an
// answer is under way, and each write puts its answer in the cache.

import { useMutation, useQueryClient } from '@tanstack/react-query';
import { keys } from './keys.ts';
import { useApi, useLiveQuery } from './provider.tsx';
import type { Engine, MemberId, ProjectId, SessionId, TaskId, TaskStatus, WorkstreamId } from './types.ts';

export interface EngineStatus {
  engine: Engine;
  /** On the PATH of the hub's own machine, where questions run. */
  installed: boolean;
}

export interface OrchestratorLimits {
  question_chars: number;
  answer_bytes: number;
  answer_seconds: number;
  turns: number;
  conversations: number;
}

export type TurnState = 'answering' | 'answered' | 'canceled' | 'timed_out' | 'too_long' | 'failed';

export type ReferenceTarget =
  | { kind: 'session'; id: SessionId }
  | { kind: 'task'; id: TaskId; key: string }
  | { kind: 'workstream'; id: WorkstreamId; project: ProjectId }
  | { kind: 'project'; id: ProjectId }
  | { kind: 'recap'; project: ProjectId; workstream?: WorkstreamId; date?: string };

export interface AnswerReference {
  /** Exactly as the answer has it. */
  text: string;
  target: ReferenceTarget;
  label: string;
}

/** Data only: nothing happens unless the person clicks it. */
export type AnswerSuggestion =
  | { kind: 'move_task'; task: TaskId; key: string; to: TaskStatus; label: string }
  | { kind: 'open'; target: ReferenceTarget; label: string };

export interface AnswerUsage {
  duration_ms: number;
  tool_runs: number;
  answer_bytes: number;
}

export interface OrchestratorTurn {
  question: string;
  asked: number;
  session: SessionId;
  state: TurnState;
  /** Untrusted text from an agent CLI: shown as text, never as markup. */
  answer: string;
  references: AnswerReference[];
  suggestions: AnswerSuggestion[];
  usage?: AnswerUsage;
  ended?: number;
  note?: string;
}

export interface Conversation {
  id: string;
  engine: Engine;
  agent: MemberId;
  started: number;
  /** The session answering it, while that lives. */
  session?: SessionId;
  turns: OrchestratorTurn[];
}

export interface Orchestrator {
  engines: EngineStatus[];
  /** The engine chosen last for a new conversation. */
  engine?: Engine;
  limits: OrchestratorLimits;
  /** Newest first. */
  conversations: Conversation[];
}

export interface Question {
  text: string;
  engine?: Engine;
  conversation?: string;
  agent?: MemberId;
}

/** How often an answer under way is looked at. */
export const ANSWER_POLL_MS = 1000;

export const answering = (o: Orchestrator | undefined): boolean =>
  o?.conversations.some((c) => c.turns.some((t) => t.state === 'answering')) === true;

/** The person's own conversations; polled while an answer is under way. */
export function useOrchestrator() {
  const api = useApi();
  return useLiveQuery({
    queryKey: keys.orchestrator,
    queryFn: ({ signal }) => api.request<Orchestrator>('GET', '/v1/orchestrator', { signal }),
    refetchInterval: (query) => (answering(query.state.data) ? ANSWER_POLL_MS : false),
  });
}

/** Puts a conversation the hub answered with into the cached state, newest first. */
function withConversation(state: Orchestrator | undefined, conversation: Conversation, engine?: Engine) {
  if (state === undefined) return state;
  const others = state.conversations.filter((c) => c.id !== conversation.id);
  return {
    ...state,
    ...(engine === undefined ? {} : { engine }),
    conversations: [conversation, ...others],
  };
}

/** Asks: a new conversation, or a follow-up in `conversation`. */
export function useAsk() {
  const api = useApi();
  const queries = useQueryClient();
  return useMutation({
    mutationFn: (question: Question) =>
      api.request<Conversation>('POST', '/v1/orchestrator/questions', { body: question }),
    onSuccess: (conversation, question) => {
      queries.setQueryData<Orchestrator>(keys.orchestrator, (state) =>
        withConversation(state, conversation, question.conversation === undefined ? conversation.engine : undefined),
      );
      void queries.invalidateQueries({ queryKey: keys.orchestrator });
    },
    onError: () => void queries.invalidateQueries({ queryKey: keys.orchestrator }),
  });
}

/** Stops the answer under way in a conversation (its CLI gets Esc). */
export function useCancelAnswer() {
  const api = useApi();
  const queries = useQueryClient();
  return useMutation({
    mutationFn: (conversation: string) =>
      api.request<Conversation>('POST', `/v1/orchestrator/conversations/${encodeURIComponent(conversation)}/cancel`),
    onSuccess: (conversation) => {
      queries.setQueryData<Orchestrator>(keys.orchestrator, (state) => withConversation(state, conversation));
    },
    onSettled: () => void queries.invalidateQueries({ queryKey: keys.orchestrator }),
  });
}

/** Forgets the person's conversations and ends their Orchestrator session. */
export function useClearConversations() {
  const api = useApi();
  const queries = useQueryClient();
  return useMutation({
    mutationFn: () => api.request<undefined>('DELETE', '/v1/orchestrator/conversations'),
    onSuccess: () => {
      queries.setQueryData<Orchestrator>(keys.orchestrator, (state) =>
        state === undefined ? state : { ...state, conversations: [] },
      );
    },
    onSettled: () => void queries.invalidateQueries({ queryKey: keys.orchestrator }),
  });
}
