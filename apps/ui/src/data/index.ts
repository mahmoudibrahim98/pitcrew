export { ApiError, createApi, GatewayError, type Api, type ApiOptions, type GatewayErrorCode } from './api.ts';
export * from './hooks.ts';
export { invalidationMap, keysToInvalidate } from './invalidation.ts';
export { keys } from './keys.ts';
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
export { AppData } from './root.tsx';
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
  useGatewayWorkspaces,
  WorkspaceScope,
  type GatewayWorkspace,
  type ScopeFallback,
  type WorkspaceList,
  type WorkspacesView,
  type WorkspaceState,
} from './workspaces.tsx';
