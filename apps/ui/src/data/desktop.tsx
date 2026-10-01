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
  /** How long a workspace may stay out of view before its stream closes. Default 10 minutes. */
  backgroundMs?: number;
  /** How long a `gateway://navigate` target may wait for the workspace list. Default 60 seconds. */
  pendingNavigateMs?: number;
}

/** The workspaces and their data scopes. No React here. */
export class Workspaces implements WorkspaceRegistry {
  readonly store: StoreApi<WorkspaceList> = createStore<WorkspaceList>(() => ({}));
  readonly #gateway: Gateway;
  readonly #createQueryClient: () => QueryClient;
  readonly #data = new Map<string, WorkspaceData>();
  /** Workspaces opened so far: their streams run, also in the background, until `stop()`. */
  readonly #open = new Set<string>();
  /** A workspace out of view, waiting on `#backgroundMs` before its stream closes. */
  readonly #leaving = new Map<string, ReturnType<typeof setTimeout>>();
  readonly #backgroundMs: number;
  #running = false;
  /** Bumped by `stop()`, so late answers from before it are ignored. */
  #generation = 0;
  #unlistenWorkspaces: (() => void) | undefined;
  #unlistenNavigate: (() => void) | undefined;
  /** A `gateway://workspaces` event arrived in this generation: it is newer than any answer. */
  #heard = false;
  readonly #listBackoff: { initialMs: number; maxMs: number };
  #listAttempt = 0;
  #listTimer: ReturnType<typeof setTimeout> | undefined;
  /** The generation whose read is out, if any. */
  #reading: number | undefined;
  /** React's own subscribers to `onNavigate()` (`useGatewayNavigate`); independent of `start()`/`stop()`. */
  readonly #navigateListeners = new Set<(target: unknown, workspaces: readonly GatewayWorkspace[]) => void>();
  /**
   * A `gateway://navigate` target that arrived before the workspace list was first known: the
   * gateway itself holds a launch-time deep link until the webview's first `gateway_workspaces()`
   * call, then emits it — which can race the very read that would answer "is this workspace
   * known?". Held here until `#update()` first learns the list, then delivered; expires after
   * `#pendingNavigateMs` (the contract's own 60 s) rather than waiting on a list that may never
   * come (the gateway cannot be reached, say).
   */
  #pendingNavigate: { target: unknown; timer: ReturnType<typeof setTimeout> } | undefined;
  readonly #pendingNavigateMs: number;

  constructor(gateway: Gateway, options: WorkspacesOptions = {}) {
    this.#gateway = gateway;
    this.#createQueryClient = options.createQueryClient ?? createQueryClient;
    this.#listBackoff = options.listBackoff ?? { initialMs: 1_000, maxMs: 30_000 };
    this.#backgroundMs = options.backgroundMs ?? 10 * 60_000;
    this.#pendingNavigateMs = options.pendingNavigateMs ?? 60_000;
  }

  start(): void {
    if (this.#running) return;
    this.#running = true;
    this.#generation += 1;
    this.#heard = false;
    this.#listAttempt = 0;
    this.#clearPendingNavigate();
    void this.#follow(this.#generation);
    for (const id of this.#open) this.#data.get(id)?.live.start();
  }

  stop(): void {
    if (!this.#running) return;
    this.#running = false;
    this.#generation += 1;
    this.#unlistenWorkspaces?.();
    this.#unlistenWorkspaces = undefined;
    this.#unlistenNavigate?.();
    this.#unlistenNavigate = undefined;
    if (this.#listTimer !== undefined) clearTimeout(this.#listTimer);
    this.#listTimer = undefined;
    this.#clearPendingNavigate();
    for (const timer of this.#leaving.values()) clearTimeout(timer);
    this.#leaving.clear();
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

  /**
   * The workspace is in view: cancels any pending background close and makes sure its stream is
   * running — resuming with `since` (the stream's own `rev`) if it had been closed for being
   * backgrounded, exactly as it would after any other disconnect.
   */
  open(id: string): void {
    this.#open.add(id);
    this.#cancelLeave(id);
    if (this.#running) this.#data.get(id)?.live.start();
  }

  /**
   * The workspace left view: after `backgroundMs` still out of view, its stream closes (the next
   * `open()` resumes it with `since`). Cancelled by `open()` on this workspace, or by its removal.
   */
  leave(id: string): void {
    this.#cancelLeave(id);
    if (!this.#running) return;
    this.#leaving.set(
      id,
      setTimeout(() => {
        this.#leaving.delete(id);
        this.#data.get(id)?.live.stop();
      }, this.#backgroundMs),
    );
  }

  /**
   * Follows `gateway://navigate`, holding a target that arrives before the workspace list is
   * known (`#pendingNavigate`) so `listener` only ever sees it with the list to check it against.
   */
  onNavigate(listener: (target: unknown, workspaces: readonly GatewayWorkspace[]) => void): Promise<() => void> {
    this.#navigateListeners.add(listener);
    return Promise.resolve(() => this.#navigateListeners.delete(listener));
  }

  /**
   * The gateway's own `gateway://navigate` event: held until the list is known, then delivered —
   * or, failing that within `#pendingNavigateMs`, dropped (the contract's own 60 s: a deep link
   * this stale is not worth guessing at by then).
   */
  #onGatewayNavigate(target: unknown): void {
    if (this.store.getState().list === undefined) {
      this.#clearPendingNavigate();
      const timer = setTimeout(() => (this.#pendingNavigate = undefined), this.#pendingNavigateMs);
      this.#pendingNavigate = { target, timer };
      return;
    }
    this.#deliverNavigate(target);
  }

  #deliverNavigate(target: unknown): void {
    const list = this.store.getState().list ?? [];
    for (const listener of this.#navigateListeners) listener(target, list);
  }

  #clearPendingNavigate(): void {
    if (this.#pendingNavigate !== undefined) {
      clearTimeout(this.#pendingNavigate.timer);
      this.#pendingNavigate = undefined;
    }
  }

  #cancelLeave(id: string): void {
    const timer = this.#leaving.get(id);
    if (timer !== undefined) {
      clearTimeout(timer);
      this.#leaving.delete(id);
    }
  }

  /**
   * Subscribes to both events first, then reads the list, so no change — and no deep link, which
   * the gateway holds until this first read — falls between subscribing and reading.
   */
  async #follow(generation: number): Promise<void> {
    const current = () => generation === this.#generation;
    try {
      const unlisten = await this.#gateway.onWorkspaces((list) => {
        if (!current()) return;
        this.#heard = true;
        this.#update(list);
      });
      if (current()) this.#unlistenWorkspaces = unlisten;
      else unlisten();
    } catch (error) {
      if (current()) console.warn('pitcrew: cannot follow the gateway’s workspaces', error);
    }
    try {
      const unlisten = await this.#gateway.onNavigate((target) => {
        if (current()) this.#onGatewayNavigate(target);
      });
      if (current()) this.#unlistenNavigate = unlisten;
      else unlisten();
    } catch (error) {
      if (current()) console.warn('pitcrew: cannot follow gateway://navigate', error);
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
    if (before === undefined && this.#pendingNavigate !== undefined) {
      const { target } = this.#pendingNavigate;
      this.#clearPendingNavigate();
      this.#deliverNavigate(target);
    }
    for (const [id, data] of this.#data) {
      const workspace = list.find((w) => w.id === id);
      if (workspace === undefined) {
        // Removed: its data goes with it.
        this.#cancelLeave(id);
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
