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
  /**
   * Follows `gateway://navigate` (a deep link, or a click on the app's own notifications);
   * `listener` gets the raw payload, unvalidated (the shell checks it: `NavigateTarget`,
   * `shell/gateway-navigate.ts`). Resolves to an unsubscribe.
   */
  onNavigate(listener: (target: unknown) => void): Promise<() => void>;
  /** The transport for a workspace; `name` gives its current name, for messages. */
  transport(id: string, name: () => string): Transport;
}

export interface WorkspaceList {
  /** Undefined until the gateway has answered. */
  list?: GatewayWorkspace[] | undefined;
  /** Why the list could not be read (it is retried, with back-off, until it is known). */
  error?: string | undefined;
}

/** The list, and a way to read it again now while it is unknown (a Retry button). */
export interface WorkspacesView extends WorkspaceList {
  retry(): void;
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
  /**
   * The workspace is in view: starts its stream (resuming with `since` if it had been closed for
   * being backgrounded) and cancels any pending background close.
   */
  open(id: string): void;
  /**
   * The workspace left view: after some time still unfocused, its stream closes, to be resumed
   * with `since` by the next `open()`.
   */
  leave(id: string): void;
  /** Reads the list again now, if it is still unknown. */
  retry(): void;
  /**
   * Follows `gateway://navigate`, only once the workspace list is known (a target that arrives
   * before then — the gateway holds a launch-time deep link until the webview's first
   * `gateway_workspaces()` call — waits for it): `listener` gets the raw target and the list to
   * check it against, together, so there is no separate read of the list that could race it.
   */
  onNavigate(listener: (target: unknown, workspaces: readonly GatewayWorkspace[]) => void): Promise<() => void>;
}

export const WorkspacesContext = createContext<WorkspaceRegistry | null>(null);
const NONE: StoreApi<WorkspaceList> = createStore<WorkspaceList>(() => ({}));

/** The gateway's workspaces in the desktop app; `null` in a browser, where the hub has one. */
export function useGatewayWorkspaces(): WorkspacesView | null {
  const workspaces = use(WorkspacesContext);
  const list = useStore(workspaces?.store ?? NONE, (s) => s.list);
  const error = useStore(workspaces?.store ?? NONE, (s) => s.error);
  return workspaces === null ? null : { list, error, retry: () => workspaces.retry() };
}

/**
 * Follows `gateway://navigate` (a deep link, or a click on the app's own notifications); a no-op
 * in a browser. `onTarget` gets the raw, unvalidated payload (the shell checks it) together with
 * the workspace list to check it against — already known by the time this fires, even for a
 * target that arrived at launch, before the gateway's own list.
 */
export function useGatewayNavigate(onTarget: (target: unknown, workspaces: readonly GatewayWorkspace[]) => void): void {
  const workspaces = use(WorkspacesContext);
  useEffect(() => {
    if (workspaces === null) return undefined;
    let cancelled = false;
    let unlisten: (() => void) | undefined;
    workspaces.onNavigate(onTarget).then(
      (unsubscribe) => (cancelled ? unsubscribe() : (unlisten = unsubscribe)),
      (error: unknown) => console.warn('pitcrew: cannot follow gateway://navigate', error),
    );
    return () => {
      cancelled = true;
      unlisten?.();
    };
  }, [workspaces, onTarget]);
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
  // With `data`: a workspace removed and listed again has new data, whose stream must start.
  // Unmounting (switching to another workspace) is leaving view: `leave()` backgrounds it.
  useEffect(() => {
    workspaces.open(id);
    return () => workspaces.leave(id);
  }, [workspaces, id, data]);
  return (
    <DataScope api={data.api} queryClient={data.queryClient} live={data.live} workspace={workspace}>
      {children}
    </DataScope>
  );
}
