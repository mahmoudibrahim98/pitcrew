// The desktop app's data layer: the gateway's workspaces, which follow `gateway://workspaces`,
// and one data scope per workspace (its API client, its own QueryClient, its stream). A query
// cache never holds two workspaces' data, so switching cannot show one workspace's in another.
//
// Loaded only in the desktop app, by dynamic import, with the gateway (`gateway.ts`) and
// `@tauri-apps/api`: none of it is in the browser's initial JS.

import type { QueryClient } from '@tanstack/react-query';
import { useEffect, useState, type ReactNode } from 'react';
import { createStore, type StoreApi } from 'zustand/vanilla';
import { createApi } from './api.ts';
import { createGateway } from './gateway.ts';
import { createLive } from './live.ts';
import { createQueryClient } from './provider.tsx';
import {
  WorkspacesContext,
  type Gateway,
  type GatewayWorkspace,
  type WorkspaceData,
  type WorkspaceList,
  type WorkspaceRegistry,
  type WorkspaceState,
} from './workspaces.tsx';

const STATES: readonly WorkspaceState[] = ['connecting', 'ready', 'unreachable', 'needs_pairing'];

function isWorkspace(value: unknown): value is GatewayWorkspace {
  if (typeof value !== 'object' || value === null) return false;
  const { id, name, state } = value as Record<string, unknown>;
  return typeof id === 'string' && id !== '' && typeof name === 'string' && STATES.includes(state as WorkspaceState);
}

function messageOf(error: unknown): string {
  if (error instanceof Error) return error.message;
  const message = typeof error === 'object' && error !== null ? (error as { message?: unknown }).message : undefined;
  return typeof message === 'string' ? message : String(error);
}

export interface WorkspacesOptions {
  createQueryClient?: () => QueryClient;
  /** While the list is unknown, a failed read is retried after `min(maxMs, initialMs * 2^n)`. */
  listBackoff?: { initialMs: number; maxMs: number };
}

/** The workspaces and their data scopes. No React here. */
export class Workspaces implements WorkspaceRegistry {
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
  /** A `gateway://workspaces` event arrived in this generation: it is newer than any answer. */
  #heard = false;
  readonly #listBackoff: { initialMs: number; maxMs: number };
  #listAttempt = 0;
  #listTimer: ReturnType<typeof setTimeout> | undefined;
  /** The generation whose read is out, if any. */
  #reading: number | undefined;

  constructor(gateway: Gateway, options: WorkspacesOptions = {}) {
    this.#gateway = gateway;
    this.#createQueryClient = options.createQueryClient ?? createQueryClient;
    this.#listBackoff = options.listBackoff ?? { initialMs: 1_000, maxMs: 30_000 };
  }

  start(): void {
    if (this.#running) return;
    this.#running = true;
    this.#generation += 1;
    this.#heard = false;
    this.#listAttempt = 0;
    void this.#follow(this.#generation);
    for (const id of this.#open) this.#data.get(id)?.live.start();
  }

  stop(): void {
    if (!this.#running) return;
    this.#running = false;
    this.#generation += 1;
    this.#unlisten?.();
    this.#unlisten = undefined;
    if (this.#listTimer !== undefined) clearTimeout(this.#listTimer);
    this.#listTimer = undefined;
    for (const data of this.#data.values()) data.live.stop();
  }

  retry(): void {
    if (!this.#running || this.store.getState().list !== undefined) return;
    if (this.#listTimer !== undefined) clearTimeout(this.#listTimer);
    this.#listTimer = undefined;
    void this.#read(this.#generation);
  }

  data(workspace: GatewayWorkspace): WorkspaceData {
    let data = this.#data.get(workspace.id);
    if (data === undefined) {
      const { id } = workspace;
      // The name in messages follows a rename.
      const name = () => this.store.getState().list?.find((w) => w.id === id)?.name ?? workspace.name;
      const transport = this.#gateway.transport(id, name);
      const api = createApi({ transport });
      const queryClient = this.#createQueryClient();
      const live = createLive({ queryClient, transport, probe: () => api.me() });
      data = { api, queryClient, live };
      this.#data.set(workspace.id, data);
    }
    return data;
  }

  open(id: string): void {
    this.#open.add(id);
    if (this.#running) this.#data.get(id)?.live.start();
  }

  /** Subscribes first, then reads the list, so no change falls between the two. */
  async #follow(generation: number): Promise<void> {
    const current = () => generation === this.#generation;
    try {
      const unlisten = await this.#gateway.onWorkspaces((list) => {
        if (!current()) return;
        this.#heard = true;
        this.#update(list);
      });
      if (current()) this.#unlisten = unlisten;
      else unlisten();
    } catch (error) {
      if (current()) console.warn('pitcrew: cannot follow the gateway’s workspaces', error);
    }
    if (current()) await this.#read(generation);
  }

  /**
   * Reads the list. The gateway emits only on changes, so while the list is unknown a failed read
   * is tried again, with back-off (or at once with `retry()`).
   */
  async #read(generation: number): Promise<void> {
    const current = () => generation === this.#generation;
    // One read at a time (the Retry button may be pressed while one is out).
    if (this.#reading === generation) return;
    this.#reading = generation;
    this.#listTimer = undefined;
    try {
      const list = await this.#gateway.workspaces();
      // An event heard meanwhile is at least as new as this answer.
      if (current() && !this.#heard) this.#update(list);
    } catch (error) {
      if (!current() || this.store.getState().list !== undefined) return;
      this.store.setState({ error: messageOf(error) });
      const { initialMs, maxMs } = this.#listBackoff;
      const delay = Math.min(maxMs, initialMs * 2 ** this.#listAttempt);
      this.#listAttempt += 1;
      this.#listTimer = setTimeout(() => void this.#read(generation), delay);
    } finally {
      if (this.#reading === generation) this.#reading = undefined;
    }
  }

  #update(received: unknown): void {
    const list = Array.isArray(received) ? received.filter(isWorkspace) : [];
    const before = this.store.getState().list;
    this.store.setState({ list, error: undefined });
    if (this.#listTimer !== undefined) clearTimeout(this.#listTimer);
    this.#listTimer = undefined;
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

/** The desktop app's data: the gateway's workspaces, each with its own data scope. */
export function WorkspacesProvider({
  gateway,
  options,
  children,
}: {
  gateway: Gateway;
  /** For tests. */
  options?: WorkspacesOptions;
  children: ReactNode;
}) {
  const [workspaces] = useState(() => new Workspaces(gateway, options));
  useEffect(() => {
    workspaces.start();
    return () => workspaces.stop();
  }, [workspaces]);
  return <WorkspacesContext value={workspaces}>{children}</WorkspacesContext>;
}

/** `<AppData>`'s desktop half, over the real gateway. */
export function DesktopData({ children }: { children: ReactNode }) {
  const [gateway] = useState(createGateway);
  return <WorkspacesProvider gateway={gateway}>{children}</WorkspacesProvider>;
}
