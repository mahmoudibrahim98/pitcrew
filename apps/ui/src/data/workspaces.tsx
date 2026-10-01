// The desktop app's workspaces: the gateway's list, which follows `gateway://workspaces`, and one
// data scope per workspace (its API client, its own QueryClient, its stream). A query cache never
// holds two workspaces' data, so switching cannot show one workspace's data in another.
//
// No Tauri code here: the gateway itself (`gateway.ts`) is loaded on demand, in the app only.

import type { QueryClient } from '@tanstack/react-query';
import { createContext, use, useEffect, useState, type ReactNode } from 'react';
import { useStore } from 'zustand';
import { createStore, type StoreApi } from 'zustand/vanilla';
import { createApi, type Api } from './api.ts';
import { createLive, type Live } from './live.ts';
import { createQueryClient, DataScope } from './provider.tsx';
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

const STATES: readonly WorkspaceState[] = ['connecting', 'ready', 'unreachable', 'needs_pairing'];

function isWorkspace(value: unknown): value is GatewayWorkspace {
  if (typeof value !== 'object' || value === null) return false;
  const { id, name, state } = value as Record<string, unknown>;
  return typeof id === 'string' && id !== '' && typeof name === 'string' && STATES.includes(state as WorkspaceState);
}

function messageOf(error: unknown): string {
  return error instanceof Error ? error.message : String(error);
}

/** The workspaces and their data scopes. No React here. */
export class Workspaces {
  readonly store: StoreApi<WorkspaceList> = createStore<WorkspaceList>(() => ({}));
  readonly #gateway: Gateway;
  readonly #createQueryClient: () => QueryClient;
  readonly #data = new Map<string, WorkspaceData>();
  /** Workspaces opened so far: their streams run, also in the background, until `stop()`. */
  readonly #open = new Set<string>();
  #running = false;
  /** Bumped by `stop()`, so late answers from before it are ignored. */
  #generation = 0;
  #unlisten: (() => void) | undefined;

  constructor(gateway: Gateway, options: { createQueryClient?: () => QueryClient } = {}) {
    this.#gateway = gateway;
    this.#createQueryClient = options.createQueryClient ?? createQueryClient;
  }

  start(): void {
    if (this.#running) return;
    this.#running = true;
    this.#generation += 1;
    void this.#follow(this.#generation);
    for (const id of this.#open) this.#data.get(id)?.live.start();
  }

  stop(): void {
    if (!this.#running) return;
    this.#running = false;
    this.#generation += 1;
    this.#unlisten?.();
    this.#unlisten = undefined;
    for (const data of this.#data.values()) data.live.stop();
  }

  /** The workspace's data, made on first use and kept while the gateway lists the workspace. */
  data(workspace: GatewayWorkspace): WorkspaceData {
    let data = this.#data.get(workspace.id);
    if (data === undefined) {
      const transport = this.#gateway.transport(workspace);
      const api = createApi({ transport });
      const queryClient = this.#createQueryClient();
      const live = createLive({ queryClient, transport, probe: () => api.me() });
      data = { api, queryClient, live };
      this.#data.set(workspace.id, data);
    }
    return data;
  }

  /** Starts the workspace's stream. It keeps its cache fresh from then on, also in the background. */
  open(id: string): void {
    this.#open.add(id);
    if (this.#running) this.#data.get(id)?.live.start();
  }

  /** Subscribes first, then reads the list, so no change falls between the two. */
  async #follow(generation: number): Promise<void> {
    const current = () => generation === this.#generation;
    let heard = false;
    try {
      const unlisten = await this.#gateway.onWorkspaces((list) => {
        if (!current()) return;
        heard = true;
        this.#update(list);
      });
      if (current()) this.#unlisten = unlisten;
      else unlisten();
    } catch (error) {
      if (current()) console.warn('pitcrew: cannot follow the gateway’s workspaces', error);
    }
    if (!current()) return;
    try {
      const list = await this.#gateway.workspaces();
      // An event heard meanwhile is at least as new as this answer.
      if (current() && !heard) this.#update(list);
    } catch (error) {
      if (current() && this.store.getState().list === undefined) {
        this.store.setState({ error: messageOf(error) });
      }
    }
  }

  #update(received: unknown): void {
    const list = Array.isArray(received) ? received.filter(isWorkspace) : [];
    const before = this.store.getState().list;
    this.store.setState({ list, error: undefined });
    for (const [id, data] of this.#data) {
      const workspace = list.find((w) => w.id === id);
      if (workspace === undefined) {
        // Removed: its data goes with it.
        data.live.stop();
        data.queryClient.clear();
        this.#data.delete(id);
        this.#open.delete(id);
      } else if (workspace.state === 'ready' && before?.find((w) => w.id === id)?.state !== 'ready') {
        // Just became ready: reconnect now, not at the next retry.
        data.live.retryNow();
      }
    }
  }
}

const WorkspacesContext = createContext<Workspaces | null>(null);
const NONE: StoreApi<WorkspaceList> = createStore<WorkspaceList>(() => ({}));

/** The desktop app's data: the gateway's workspaces, each with its own data scope. */
export function WorkspacesProvider({ gateway, children }: { gateway: Gateway; children: ReactNode }) {
  const [workspaces] = useState(() => new Workspaces(gateway));
  useEffect(() => {
    workspaces.start();
    return () => workspaces.stop();
  }, [workspaces]);
  return <WorkspacesContext value={workspaces}>{children}</WorkspacesContext>;
}

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
  workspaces: Workspaces;
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
