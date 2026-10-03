// @vitest-environment happy-dom
// @vitest-environment-options {"url": "http://localhost:5173/"}

import { setTimeout as delay } from 'node:timers/promises';
import { act, cleanup, screen, within } from '@testing-library/react';
import { afterEach, beforeEach, expect, it, vi } from 'vitest';
import { Home } from '../home.tsx';
import { ReadScope } from '../read-scope.tsx';
import { demo, otherClient, renderWithHub, startHub, stopHub, type Hub } from './harness.tsx';

let hub: Hub;
beforeEach(async () => { hub = await startHub(); });
afterEach(async () => {
  cleanup();
  vi.useRealTimers();
  await stopHub(hub);
});

// Let real network I/O and React settle without advancing the fake dwell clock.
async function settled(check: () => void | Promise<void>) {
  const deadline = performance.now() + 5000;
  for (;;) {
    await act(async () => { await delay(10); });
    try {
      await check();
      return;
    } catch (error) {
      if (performance.now() >= deadline) throw error;
    }
  }
}

it('a cursor moved on another device refreshes Home without reloading', async () => {
  renderWithHub(<Home />, hub);
  const region = await screen.findByRole('region', { name: 'Since you last looked' });
  await within(region).findByText('15 new changes');
  await otherClient(hub).request('PUT', '/v1/me/cursors/workspace', { body: { rev: 15 } });
  await within(region).findByText('Nothing new since you last looked.');
  await otherClient(hub).moveTask('PAP-2', 'in_progress');
  await within(region).findByText('1 new change');
});

it('leaving a scope before the dwell does not mark it read', async () => {
  vi.useFakeTimers({ toFake: ['setTimeout', 'clearTimeout'] });
  const view = renderWithHub(<ReadScope scope={`project:${demo.paper}`} />, hub);
  await settled(() => {
    expect(view.queryClient.getQueryData(['cursors'])).toEqual([]);
    expect(view.queryClient.isFetching()).toBe(0);
  });
  await act(async () => { await vi.advanceTimersByTimeAsync(999); });
  view.unmount();
  await act(async () => { await vi.advanceTimersByTimeAsync(1100); });
  expect(await otherClient(hub).request('GET', '/v1/me/cursors')).toEqual([]);
});

it('marks a visited scope once and leaves later live changes unread', async () => {
  const scope = `workstream:${demo.submission}`;
  vi.useFakeTimers({ toFake: ['setTimeout', 'clearTimeout'] });
  const view = renderWithHub(<ReadScope scope={scope} />, hub);
  await settled(() => {
    expect(view.queryClient.getQueryData(['cursors'])).toEqual([]);
    expect(view.queryClient.isFetching()).toBe(0);
  });
  await act(async () => { await vi.advanceTimersByTimeAsync(999); });
  expect(await otherClient(hub).request('GET', '/v1/me/cursors')).toEqual([]);
  await act(async () => { await vi.advanceTimersByTimeAsync(1); });
  await settled(async () => expect(await otherClient(hub).request('GET', '/v1/me/cursors')).toEqual([{ scope, rev: 15 }]));
  await otherClient(hub).moveTask('PAP-2', 'in_progress');
  await settled(async () => {
    await act(async () => { await vi.advanceTimersByTimeAsync(250); });
    expect(view.queryClient.getQueriesData<{ to_rev: number }>({ queryKey: ['events'] })
      .some(([, page]) => page?.to_rev === 17)).toBe(true);
  });
  await act(async () => { await vi.advanceTimersByTimeAsync(1100); });
  expect(await otherClient(hub).request('GET', '/v1/me/cursors')).toEqual([{ scope, rev: 15 }]);
});

it('aborts a pending dwell write when the scope unmounts', async () => {
  vi.useFakeTimers({ toFake: ['setTimeout', 'clearTimeout'] });
  let signal: AbortSignal | null | undefined;
  const pendingFetch: typeof fetch = (input, init) => {
    if (init?.method !== 'PUT') return fetch(input, init);
    signal = init.signal;
    return new Promise((_resolve, reject) => {
      signal?.addEventListener('abort', () => reject(new DOMException('Aborted', 'AbortError')), { once: true });
    });
  };
  const view = renderWithHub(<ReadScope scope={`project:${demo.paper}`} />, hub, { fetch: pendingFetch });
  await settled(() => {
    expect(view.queryClient.getQueryData(['cursors'])).toEqual([]);
    expect(view.queryClient.isFetching()).toBe(0);
  });
  await act(async () => { await vi.advanceTimersByTimeAsync(1000); });
  await settled(() => expect(signal?.aborted).toBe(false));
  view.unmount();
  expect(signal?.aborted).toBe(true);
  await settled(() => expect(view.queryClient.isMutating()).toBe(0));
  expect(await otherClient(hub).request('GET', '/v1/me/cursors')).toEqual([]);
});
