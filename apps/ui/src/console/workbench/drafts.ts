// Unsaved edits to files open in the workbench, by tab, in memory only. A tab off screen is not
// rendered, so its viewer cannot hold the edit itself: the draft waits here, with the revision it
// was made from, and a save still sends that revision (a change made meanwhile is a conflict, as in
// the Files tab). Nothing here is ever written to storage; a reload warns first.

import { useSyncExternalStore } from 'react';
import type { FileDraft } from '../../projects/index.ts';

const drafts = new Map<string, FileDraft>();
const listeners = new Set<() => void>();
let version = 0;

const key = (ws: string, tab: string) => `${ws}\u0000${tab}`;

function changed(): void {
  version += 1;
  for (const listener of listeners) listener();
}

export function getDraft(ws: string, tab: string): FileDraft | undefined {
  return drafts.get(key(ws, tab));
}

export function setDraft(ws: string, tab: string, draft: FileDraft | undefined): void {
  const k = key(ws, tab);
  if (drafts.get(k) === draft) return;
  if (draft === undefined) drafts.delete(k);
  else drafts.set(k, draft);
  changed();
}

/** Whether the tab holds an edit that differs from what it was made from. */
export function isDirty(ws: string, tab: string): boolean {
  const draft = drafts.get(key(ws, tab));
  return draft !== undefined && draft.text !== draft.base;
}

/** The tabs of workspace `ws` with unsaved edits. */
export function dirtyTabs(ws: string): string[] {
  const prefix = `${ws}\u0000`;
  return [...drafts.entries()]
    .filter(([k, d]) => k.startsWith(prefix) && d.text !== d.base)
    .map(([k]) => k.slice(prefix.length));
}

export function forgetDraft(ws: string, tab: string): void {
  setDraft(ws, tab, undefined);
}

function subscribe(listener: () => void): () => void {
  listeners.add(listener);
  return () => listeners.delete(listener);
}

/** Re-renders when any draft changes. */
export function useDrafts(): number {
  return useSyncExternalStore(subscribe, () => version);
}

/** For tests. */
export function clearDrafts(): void {
  drafts.clear();
  changed();
}
