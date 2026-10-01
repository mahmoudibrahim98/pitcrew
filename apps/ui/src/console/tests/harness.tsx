/// <reference types="node" />
// Test harness: the real mock hub (a child process on a free port, as L's DOM tests run it, since
// happy-dom replaces globals the in-process hub relies on), a DataProvider around the component,
// a log of every request, and a stand-in layout so virtualised lists have a size in happy-dom.

import { cleanup, configure, render } from '@testing-library/react';
import { StrictMode, type ReactNode } from 'react';
import { vi } from 'vitest';
import { transcriptPage, type TranscriptRecord } from '../../../../mock-hub/src/transcripts.ts';
import { freePort, spawnHub, type HubProcess } from '../../../tests/hub-process.ts';
import { createApi, createQueryClient, DataProvider, type Session } from '../../data/index.ts';

export const DEVICE_TOKEN = 'dev-device-token';

// Everything here goes through a real hub process and its stream; on a busy machine the default
// one second for `findBy…` is too short.
const PATIENCE = 5_000;
configure({ asyncUtilTimeout: PATIENCE });

/** `vi.waitFor` with the same patience as `findBy…`. */
export function eventually<T>(check: () => T | Promise<T>, options: { timeout?: number } = {}): Promise<T> {
  return vi.waitFor(check, { timeout: PATIENCE, ...options });
}

export const ID = {
  person: '01JB000000000000000MEM0001',
  writer: '01JB000000000000000MEM0002',
  laptop: '01JB000000000000000MCH0001',
  gpuBox: '01JB000000000000000MCH0003',
  ses1: '01JB000000000000000SES0001',
  ses2: '01JB000000000000000SES0002',
  ses3: '01JB000000000000000SES0003',
  ses4: '01JB000000000000000SES0004',
  ses5: '01JB000000000000000SES0005',
  ses6: '01JB000000000000000SES0006',
  ask1: '01JB000000000000000ASK0001',
} as const;

export type { HubProcess };

export async function startHub(): Promise<HubProcess> {
  return spawnHub(await freePort());
}

export interface Logged {
  method: string;
  path: string;
  query: URLSearchParams;
  body: unknown;
}

const inFlight = new Set<Promise<unknown>>();

/**
 * Unmounts, then waits for requests still in flight, so stopping the hub next does not reset
 * them (happy-dom's fetch would print each reset to the real stderr).
 */
export async function unmountAndSettle(): Promise<void> {
  cleanup();
  const deadline = new Promise((done) => setTimeout(done, 2_000));
  await Promise.race([Promise.allSettled([...inFlight]), deadline]);
  await new Promise((done) => setTimeout(done, 20));
}

/**
 * Renders `ui` in a `DataProvider` on the hub. `strict` puts Strict Mode at the root, as
 * src/main.tsx does: only there does React run effects, undo them and run them again on mount (a
 * `<StrictMode>` lower in the tree, under this provider, does not).
 */
export function renderWithHub(
  hub: { url: string },
  ui: ReactNode,
  options: { fetch?: typeof fetch; strict?: boolean } = {},
) {
  const requests: Logged[] = [];
  const inner = options.fetch ?? ((input: RequestInfo | URL, init?: RequestInit) => fetch(input, init));
  const fetcher: typeof fetch = (input, init) => {
    const url = new URL(String(input));
    requests.push({
      method: init?.method ?? 'GET',
      path: url.pathname,
      query: url.searchParams,
      body: typeof init?.body === 'string' ? JSON.parse(init.body) : undefined,
    });
    const response = inner(input, init);
    inFlight.add(response);
    void response.then(
      () => inFlight.delete(response),
      () => inFlight.delete(response),
    );
    return response;
  };
  const api = createApi({ baseUrl: hub.url, token: DEVICE_TOKEN, fetch: fetcher });
  const queryClient = createQueryClient();
  const tree = (
    <DataProvider api={api} queryClient={queryClient} token={DEVICE_TOKEN}>
      {ui}
    </DataProvider>
  );
  const result = render(options.strict === true ? <StrictMode>{tree}</StrictMode> : tree);
  return { ...result, api, queryClient, requests };
}

/** A made-up session the hub does not know, served by `serveSynthetic`. */
export const SYNTHETIC = '01JB0000000000000000SYNTH1';

export function syntheticSession(): Session {
  return {
    id: SYNTHETIC,
    engine: 'claude',
    native_id: 'synthetic',
    machine: ID.laptop,
    cwd: '/work/synthetic',
    title: 'Synthetic',
    state: 'idle',
    started: 0,
    last_activity: 0,
  };
}

const json = (body: unknown) =>
  new Response(JSON.stringify(body), { status: 200, headers: { 'Content-Type': 'application/json' } });

/**
 * Serves the synthetic session and `records` as its transcript, with the mock hub's own paging;
 * everything else goes to `next` (the hub, by default). The session never changes.
 */
export function serveSynthetic(records: TranscriptRecord[], next: typeof fetch = fetch): typeof fetch {
  const session = syntheticSession();
  return (input, init) => {
    const url = new URL(String(input));
    if (url.pathname === `/v1/sessions/${SYNTHETIC}/transcript`) {
      const before = url.searchParams.get('before');
      const limit = url.searchParams.get('limit');
      return Promise.resolve(
        json(transcriptPage(records, before === null ? undefined : Number(before), limit === null ? 200 : Number(limit))),
      );
    }
    if (url.pathname === `/v1/sessions/${SYNTHETIC}`) return Promise.resolve(json(session));
    // Send, keys, interrupt and end are accepted and do nothing.
    if (init?.method === 'POST' && url.pathname.startsWith(`/v1/sessions/${SYNTHETIC}/`)) {
      return Promise.resolve(new Response(null, { status: 204 }));
    }
    return next(input, init);
  };
}

/** Holds every request for `path` until `release()`; the rest go to `next` (the hub, by default). */
export function holdPath(path: string, next: typeof fetch = fetch): { fetch: typeof fetch; release(): void } {
  let release = () => {};
  const gate = new Promise<void>((resolve) => {
    release = resolve;
  });
  return {
    fetch: async (input, init) => {
      if (new URL(String(input)).pathname === path) await gate;
      return next(input, init);
    },
    release,
  };
}

/** Another client of the same hub, to change things behind the UI's back. */
export function otherClient(hub: { url: string }) {
  return createApi({ baseUrl: hub.url, token: DEVICE_TOKEN });
}

const Element = globalThis.HTMLElement.prototype;

function heightOf(element: HTMLElement, sizes: LayoutSizes): number {
  if (element.hasAttribute('data-virtual-scroller')) return sizes.viewport;
  if (element.hasAttribute('data-index')) return sizes.row;
  return 0;
}

export interface LayoutSizes {
  /** The scroll container's height. */
  viewport: number;
  /** Every virtual row's height. */
  row: number;
}

/**
 * Gives virtual scrollers and their rows a size (happy-dom lays nothing out), and makes scrolling
 * a scroller clamp and fire `scroll` as a browser would. Returns a function that undoes it.
 */
export function stubLayout(sizes: LayoutSizes = { viewport: 600, row: 40 }): () => void {
  const saved = (['offsetHeight', 'offsetWidth', 'clientHeight', 'scrollHeight', 'scroll'] as const).map(
    (name) => [name, Object.getOwnPropertyDescriptor(Element, name)] as const,
  );
  const define = (name: string, get: (this: HTMLElement) => number) =>
    Object.defineProperty(Element, name, { configurable: true, get });
  define('offsetHeight', function () {
    return heightOf(this, sizes);
  });
  define('offsetWidth', function () {
    return this.hasAttribute('data-virtual-scroller') || this.hasAttribute('data-index') ? 480 : 0;
  });
  define('clientHeight', function () {
    return this.hasAttribute('data-virtual-scroller') ? sizes.viewport : 0;
  });
  define('scrollHeight', function () {
    if (!this.hasAttribute('data-virtual-scroller')) return 0;
    const inner = this.firstElementChild as HTMLElement | null;
    return Math.max(sizes.viewport, Number.parseFloat(inner?.style.height ?? '0') || 0);
  });
  // Possibly inherited from Element.prototype; restored by deleting the own property below.
  const scroll = Element.scroll as (this: HTMLElement, options: ScrollToOptions) => void;
  Object.defineProperty(Element, 'scroll', {
    configurable: true,
    writable: true,
    value(this: HTMLElement, options: ScrollToOptions) {
      if (!this.hasAttribute('data-virtual-scroller')) return scroll.call(this, options);
      const max = Math.max(0, this.scrollHeight - this.clientHeight);
      this.scrollTop = Math.min(max, Math.max(0, Number(options.top ?? this.scrollTop)));
      setTimeout(() => this.dispatchEvent(new Event('scroll')), 0);
    },
  });
  Object.defineProperty(Element, 'scrollTo', {
    configurable: true,
    writable: true,
    value(this: HTMLElement, options: ScrollToOptions) {
      this.scroll(options);
    },
  });
  return () => {
    for (const [name, descriptor] of saved) {
      if (descriptor === undefined) Reflect.deleteProperty(Element, name);
      else Object.defineProperty(Element, name, descriptor);
    }
    Reflect.deleteProperty(Element, 'scrollTo');
  };
}

/** Scrolls a virtual scroller to `top` as a user would, firing `scroll`. */
export function scrollTo(scroller: HTMLElement, top: number): void {
  scroller.scrollTop = top;
  scroller.dispatchEvent(new Event('scroll'));
}
