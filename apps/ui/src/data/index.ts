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
  useLiveQuery,
  useOpenSocket,
} from './provider.tsx';
export { AppData } from './root.tsx';
export { streamPath, terminalPath, type StreamStatus } from './stream.ts';
export {
  browserTransport,
  isDesktop,
  type BrowserOptions,
  type Method,
  type SocketClose,
  type Transport,
  type TransportResponse,
  type TransportSocket,
} from './transport.ts';
export type * from './types.ts';
export { TRANSCRIPT_KINDS } from './types.ts';
export {
  useGatewayWorkspaces,
  WorkspaceScope,
  type GatewayWorkspace,
  type ScopeFallback,
  type WorkspaceList,
  type WorkspaceState,
} from './workspaces.tsx';
