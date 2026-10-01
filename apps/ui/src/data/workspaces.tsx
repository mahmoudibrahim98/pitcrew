// What the shell sees of the desktop app's workspaces: the gateway's list, and each workspace's
// data scope. The registry behind it (`desktop.tsx`) is loaded only in the desktop app; in a
// browser there is none, and these hooks fall back to the hub's one workspace.

import type { QueryClient } from '@tanstack/react-query';
import { createContext, use, useEffect, type ReactNode } from 'react';
import { useStore } from 'zustand';
import { createStore, type StoreApi } from 'zustand/vanilla';
import type { Api } from './api.ts';
import type { Live } from './live.ts';
import { DataScope } from './provider.tsx';
import type { Transport } from './transport.ts';

export type WorkspaceState = 'connecting' | 'ready' | 'unreachable' | 'needs_pairing';

/** A workspace the gateway knows (`gateway_workspaces()`). */
export interface GatewayWorkspace {
  /** The daemon's workspace id (a ULID), as in `/w/$ws/…`. */
  id: string;
  name: string;
  kind: 'local' | 'remote';
  state: WorkspaceState;
  /** Why it is unreachable or needs pairing, for people to read. */
  detail?: string | undefined;
}

/** The gateway as the data layer uses it: `gateway.ts` makes the real one, tests a fake. */
export interface Gateway {
  workspaces(): Promise<GatewayWorkspace[]>;
  /** Follows `gateway://workspaces`; `listener` gets the whole list. Resolves to an unsubscribe. */
  onWorkspaces(listener: (workspaces: GatewayWorkspace[]) => void): Promise<() => void>;
  transport(workspace: GatewayWorkspace): Transport;
}

export interface WorkspaceList {
  /** Undefined until the gateway has answered. */
  list?: GatewayWorkspace[] | undefined;
  /** Why the list could not be read. */
  error?: string | undefined;
}

/** One workspace's data: what `DataProvider` gives a browser's one hub. */
export interface WorkspaceData {
  api: Api;
  queryClient: QueryClient;
  live: Live;
}

/** The desktop app's workspaces (`Workspaces` in `desktop.tsx`). */
export interface WorkspaceRegistry {
  readonly store: StoreApi<WorkspaceList>;
  /** The workspace's data, made on first use and kept while the gateway lists the workspace. */
  data(workspace: GatewayWorkspace): WorkspaceData;
  /** Starts the workspace's stream. It keeps its cache fresh from then on, also in the background. */
  open(id: string): void;
}

export const WorkspacesContext = createContext<WorkspaceRegistry | null>(null);
const NONE: StoreApi<WorkspaceList> = createStore<WorkspaceList>(() => ({}));

/** The gateway's workspaces in the desktop app; `null` in a browser, where the hub has one. */
export function useGatewayWorkspaces(): WorkspaceList | null {
  const workspaces = use(WorkspacesContext);
  const list = useStore(workspaces?.store ?? NONE, (s) => s.list);
  const error = useStore(workspaces?.store ?? NONE, (s) => s.error);
  return workspaces === null ? null : { list, error };
}

/** What `WorkspaceScope` shows when it has no workspace to give. */
export type ScopeFallback = { kind: 'loading' } | { kind: 'failed'; message: string } | { kind: 'unknown' };

/**
 * Gives `children` the data of workspace `ws`. In a browser that is the `DataProvider` above (the
 * hub's one workspace). In the desktop app each workspace has its own API client, cache and
 * stream. Key it by `ws`: everything under it must remount when the workspace changes.
 */
export function WorkspaceScope({
  ws,
  fallback,
  children,
}: {
  ws: string;
  fallback: (reason: ScopeFallback) => ReactNode;
  children: ReactNode;
}) {
  const workspaces = use(WorkspacesContext);
  const list = useStore(workspaces?.store ?? NONE, (s) => s.list);
  const error = useStore(workspaces?.store ?? NONE, (s) => s.error);
  if (workspaces === null) return children;
  const workspace = list?.find((w) => w.id === ws);
  if (workspace === undefined) {
    if (list !== undefined) return fallback({ kind: 'unknown' });
    return fallback(error === undefined ? { kind: 'loading' } : { kind: 'failed', message: error });
  }
  return (
    <OpenScope workspaces={workspaces} workspace={workspace}>
      {children}
    </OpenScope>
  );
}

function OpenScope({
  workspaces,
  workspace,
  children,
}: {
  workspaces: WorkspaceRegistry;
  workspace: GatewayWorkspace;
  children: ReactNode;
}) {
  const data = workspaces.data(workspace);
  const { id } = workspace;
  useEffect(() => workspaces.open(id), [workspaces, id]);
  return (
    <DataScope api={data.api} queryClient={data.queryClient} live={data.live} workspace={workspace}>
      {children}
    </DataScope>
  );
}
