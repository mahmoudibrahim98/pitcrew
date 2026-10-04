// The workbench's layout per workspace, kept in this browser's local storage
// (`pitcrew.workbench.<workspace>`), so it survives a reload. A stored layout that is missing,
// unreadable or malformed gives the empty layout; storage that refuses to save (full, or blocked)
// leaves the layout working for this page. No file contents are ever stored: tabs hold where a
// file is, and unsaved edits stay in memory (`drafts.ts`).

import { useSyncExternalStore } from 'react';
import { emptyLayout, parseLayout, toStored, type Layout } from './layout.ts';

export const STORAGE_PREFIX = 'pitcrew.workbench.';

export interface WorkbenchStore {
  get(): Layout;
  /** Applies `change`; saves and notifies only when it changed something. */
  update(change: (layout: Layout) => Layout): Layout;
  subscribe(listener: () => void): () => void;
}

function storage(): Storage | undefined {
  try {
    return globalThis.localStorage;
  } catch {
    // A browser that forbids storage throws on the property itself.
    return undefined;
  }
}

/** The stored layout for workspace `ws`, or the empty layout. Never throws. */
export function loadLayout(ws: string): Layout {
  try {
    const raw = storage()?.getItem(STORAGE_PREFIX + ws);
    if (raw === null || raw === undefined) return emptyLayout();
    return parseLayout(JSON.parse(raw)) ?? emptyLayout();
  } catch {
    return emptyLayout();
  }
}

export function saveLayout(ws: string, layout: Layout): void {
  try {
    storage()?.setItem(STORAGE_PREFIX + ws, JSON.stringify(toStored(layout)));
  } catch {
    // Full or blocked: the layout still works until the page goes.
  }
}

export function createStore(ws: string): WorkbenchStore {
  let layout = loadLayout(ws);
  const listeners = new Set<() => void>();
  return {
    get: () => layout,
    update(change) {
      const next = change(layout);
      if (next === layout) return layout;
      layout = next;
      saveLayout(ws, layout);
      for (const listener of listeners) listener();
      return layout;
    },
    subscribe(listener) {
      listeners.add(listener);
      return () => listeners.delete(listener);
    },
  };
}

const stores = new Map<string, WorkbenchStore>();

/** One store per workspace for the page's lifetime: the console remounting keeps its layout. */
export function workbenchStore(ws: string): WorkbenchStore {
  let store = stores.get(ws);
  if (store === undefined) {
    store = createStore(ws);
    stores.set(ws, store);
  }
  return store;
}

/** For tests: forget every workspace's store, so the next one reads storage afresh. */
export function resetWorkbenchStores(): void {
  stores.clear();
}

export function useWorkbenchLayout(store: WorkbenchStore): Layout {
  return useSyncExternalStore(store.subscribe, store.get);
}
