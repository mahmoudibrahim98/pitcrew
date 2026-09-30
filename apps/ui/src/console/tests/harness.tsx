/// <reference types="node" />
// Test harness: the real mock hub (a child process on a free port, as L's DOM tests run it, since
// happy-dom replaces globals the in-process hub relies on), a DataProvider around the component,
// a log of every request, and a stand-in layout so virtualised lists have a size in happy-dom.

import { cleanup, configure, render } from '@testing-library/react';
import type { ReactNode } from 'react';
import { vi } from 'vitest';
import { freePort, spawnHub, type HubProcess } from '../../../tests/hub-process.ts';
import { createApi, createQueryClient, DataProvider } from '../../data/index.ts';

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

export function renderWithHub(hub: { url: string }, ui: ReactNode, options: { fetch?: typeof fetch } = {}) {
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
  const result = render(
    <DataProvider api={api} queryClient={queryClient} token={DEVICE_TOKEN}>
      {ui}
    </DataProvider>,
  );
  return { ...result, api, queryClient, requests };
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
