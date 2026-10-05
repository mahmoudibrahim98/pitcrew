export { ApiError, createApi, GatewayError, type Api, type ApiOptions, type GatewayErrorCode } from './api.ts';
export * from './hooks.ts';
export { fileClient, type FileContent, type FileListing } from './files.ts';
export { useMoveCursor, useReadCursors, type ReadCursor } from './cursors.ts';
export { invalidationMap, keysToInvalidate } from './invalidation.ts';
export { keys } from './keys.ts';
export {
  answering,
  ANSWER_POLL_MS,
  useAsk,
  useCancelAnswer,
  useClearConversations,
  useOrchestrator,
  type AnswerReference,
  type AnswerSuggestion,
  type AnswerUsage,
  type Conversation,
  type EngineStatus,
  type Orchestrator,
  type OrchestratorLimits,
  type OrchestratorTurn,
  type Question,
  type ReferenceTarget,
  type TurnState,
} from './orchestrator.ts';
export type { LiveProblem } from './live.ts';
export {
  createQueryClient,
  DataProvider,
  useApi,
  useConnection,
  useGatewayWorkspace,
  useLiveInfiniteQuery,
  useLiveQuery,
  useOpenSocket,
} from './provider.tsx';
export { clauses, useRecapBlocks, useRecapDays, type Clause, type RecapBlocksOptions, type RecapDaysOptions } from './recaps.ts';
export type {
  GatewayPrompt,
  JobOptions,
  PromptKind,
  PromptReply,
  RemoteGateway,
  RemoteLauncher,
  RemotePlan,
  RemotePlanRequest,
  RemoteProbe,
  RemoteProgress,
} from './remote.ts';
export { AppData } from './root.tsx';
export { setUp, SetupConflict, useSetUp, useSetup } from './setup.ts';
export { streamPath, terminalPath, type StreamStatus } from './stream.ts';
export {
  isDesktop,
  type Method,
  type SocketClose,
  type Transport,
  type TransportResponse,
  type TransportSocket,
} from './transport.ts';
export type * from './types.ts';
export { CHECKS, TRANSCRIPT_KINDS } from './types.ts';
export {
  useGatewayNavigate,
  useGatewayPrompts,
  useGatewayWorkspaces,
  useRemoteGateway,
  WorkspaceScope,
  type GatewayPrompts,
  type GatewayWorkspace,
  type ScopeFallback,
  type WorkspaceList,
  type WorkspacesView,
  type WorkspaceState,
} from './workspaces.tsx';
