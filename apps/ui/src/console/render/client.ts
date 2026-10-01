// Parses markdown and diffs in a web worker and caches the results, so a row that scrolls back
// into view renders at once. Where no worker can run (tests, a blocked worker), the parser is
// loaded on the main thread on first use instead; the first paint never waits for it.

import { useEffect, useSyncExternalStore } from 'react';
import type { ParseKind, ParseRequest, ParseResponse, ParseResults } from './parse.ts';

/** Parsed texts kept; the oldest go first. */
const MAX_ENTRIES = 2_000;

const cache = new Map<string, ParseResults[ParseKind]>();
const listeners = new Map<string, Set<() => void>>();
const inflight = new Map<string, { kind: ParseKind; text: string }>();
const waiting = new Map<number, string>();
let nextId = 0;
/** `undefined` until first needed; `null` when the worker is unavailable. */
let worker: Worker | null | undefined;
let workerAllowed = true;

const keyOf = (kind: ParseKind, text: string): string => `${kind}\u0000${text}`;

/** For tests: parse on the main thread even where `Worker` exists. */
export function setWorkerAllowed(allowed: boolean): void {
  workerAllowed = allowed;
  worker?.terminate();
  worker = undefined;
}

function store(key: string, value: ParseResults[ParseKind]): void {
  inflight.delete(key);
  cache.set(key, value);
  if (cache.size > MAX_ENTRIES) {
    const oldest = cache.keys().next().value;
    if (oldest !== undefined) cache.delete(oldest);
  }
  for (const listener of listeners.get(key) ?? []) listener();
}

async function parseHere(key: string): Promise<void> {
  const job = inflight.get(key);
  if (job === undefined) return;
  const { parseAny } = await import('./parse.ts');
  try {
    store(key, parseAny(job.kind, job.text));
  } catch (error) {
    inflight.delete(key);
    console.warn('pitcrew: could not parse', error);
  }
}

function workerFailed(): void {
  worker?.terminate();
  worker = null;
  const keys = [...waiting.values()];
  waiting.clear();
  for (const key of keys) void parseHere(key);
}

function getWorker(): Worker | null {
  if (worker !== undefined) return worker;
  if (!workerAllowed || typeof Worker !== 'function') return (worker = null);
  try {
    const created = new Worker(new URL('./worker.ts', import.meta.url), {
      type: 'module',
      name: 'pitcrew-render',
    });
    created.onmessage = (event: MessageEvent<ParseResponse>) => {
      const key = waiting.get(event.data.id);
      waiting.delete(event.data.id);
      if (key === undefined) return;
      if ('result' in event.data) store(key, event.data.result);
      else void parseHere(key);
    };
    created.onerror = workerFailed;
    worker = created;
  } catch {
    worker = null;
  }
  return worker;
}

/** Starts parsing `text` unless it is cached or under way. */
export function requestParse(kind: ParseKind, text: string): void {
  const key = keyOf(kind, text);
  if (cache.has(key) || inflight.has(key)) return;
  inflight.set(key, { kind, text });
  const target = getWorker();
  if (target === null) {
    void parseHere(key);
    return;
  }
  const id = nextId++;
  waiting.set(id, key);
  target.postMessage({ id, kind, text } satisfies ParseRequest);
}

export function peekParsed<K extends ParseKind>(kind: K, text: string): ParseResults[K] | undefined {
  return cache.get(keyOf(kind, text)) as ParseResults[K] | undefined;
}

function subscribe(key: string, listener: () => void): () => void {
  let set = listeners.get(key);
  if (set === undefined) {
    set = new Set();
    listeners.set(key, set);
  }
  set.add(listener);
  return () => {
    set.delete(listener);
    if (set.size === 0) listeners.delete(key);
  };
}

/** The parsed form of `text`, or undefined while it is being parsed. */
export function useParsed<K extends ParseKind>(kind: K, text: string): ParseResults[K] | undefined {
  const key = keyOf(kind, text);
  const value = useSyncExternalStore(
    (listener) => subscribe(key, listener),
    () => cache.get(key) as ParseResults[K] | undefined,
  );
  useEffect(() => {
    if (value === undefined) requestParse(kind, text);
  }, [kind, text, value]);
  return value;
}
