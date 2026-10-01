// @vitest-environment happy-dom

// Hook-level coverage of `useRecapBlocks`/`useRecapDays` against the real mock hub: paging keeps
// going until `at_start`, in a child process (`hub-process.ts`) since the in-process mock hub does
// not get on with happy-dom's replaced globals.

import { act, cleanup, renderHook, waitFor } from '@testing-library/react';
import type { ReactNode } from 'react';
import { afterEach, describe, expect, it } from 'vitest';
import { createApi } from '../src/data/api.ts';
import { createQueryClient, DataProvider } from '../src/data/provider.tsx';
import { clauses, useRecapBlocks, useRecapDays } from '../src/data/recaps.ts';
import { freePort, spawnHub, type HubProcess } from './hub-process.ts';

const DEVICE_TOKEN = 'dev-device-token';
const PROJECT = '01JB000000000000000PRJ0001';

describe('useRecapBlocks and useRecapDays', () => {
  let hub: HubProcess | undefined;

  afterEach(async () => {
    cleanup();
    await hub?.close();
    hub = undefined;
  });

  it('pages blocks backwards by the last block id, all the way to at_start', async () => {
    hub = await spawnHub(await freePort());
    const api = createApi({ baseUrl: hub.url, token: DEVICE_TOKEN });
    const queryClient = createQueryClient();
    const wrapper = ({ children }: { children: ReactNode }) => (
      <DataProvider api={api} queryClient={queryClient} token={DEVICE_TOKEN}>
        {children}
      </DataProvider>
    );

    // The ground truth: every block in one page, newest first.
    const whole = await api.recapBlocks({}, undefined, 200);
    expect(whole.at_start).toBe(true);
    expect(whole.blocks.length).toBeGreaterThan(2); // the test needs more than one small page

    const hook = renderHook(() => useRecapBlocks({}, { limit: 2 }), { wrapper });
    await waitFor(() => expect(hook.result.current.blocks.length).toBeGreaterThan(0));
    expect(hook.result.current.atStart).toBe(false);

    while (!hook.result.current.atStart) {
      const before = hook.result.current.blocks.length;
      act(() => hook.result.current.loadMore());
      await waitFor(() => expect(hook.result.current.blocks.length).toBeGreaterThan(before));
    }
    expect(hook.result.current.blocks.map((b) => b.block.id)).toEqual(whole.blocks.map((b) => b.block.id));
    // Newest first, loadMore() a no-op once at_start.
    const atStart = hook.result.current.blocks.length;
    act(() => hook.result.current.loadMore());
    await new Promise((r) => setTimeout(r, 50));
    expect(hook.result.current.blocks.length).toBe(atStart);
  }, 20_000);

  it('pages days backwards by the last date, all the way to at_start', async () => {
    hub = await spawnHub(await freePort());
    const api = createApi({ baseUrl: hub.url, token: DEVICE_TOKEN });
    const queryClient = createQueryClient();
    const wrapper = ({ children }: { children: ReactNode }) => (
      <DataProvider api={api} queryClient={queryClient} token={DEVICE_TOKEN}>
        {children}
      </DataProvider>
    );

    const whole = await api.recapDays({ project: PROJECT }, 0, undefined, 30);
    expect(whole.at_start).toBe(true);
    const dates = [...new Set(whole.days.map((d) => d.date))];
    expect(dates.length).toBeGreaterThan(1); // the test needs more than one small page of dates

    // The mock only has days for tz=0: pass the override explicitly, as e2e suites must.
    const hook = renderHook(() => useRecapDays({ project: PROJECT }, { tz: 0, limit: 1 }), { wrapper });
    await waitFor(() => expect(hook.result.current.days.length).toBeGreaterThan(0));
    expect(hook.result.current.atStart).toBe(false);

    while (!hook.result.current.atStart) {
      const before = hook.result.current.days.length;
      act(() => hook.result.current.loadMore());
      await waitFor(() => expect(hook.result.current.days.length).toBeGreaterThan(before));
    }
    expect(hook.result.current.days).toEqual(whole.days);
  }, 20_000);

  it('runs no request before the stream syncs, like every other data hook', async () => {
    hub = await spawnHub(await freePort());
    const api = createApi({ baseUrl: hub.url, token: DEVICE_TOKEN });
    const queryClient = createQueryClient();
    const wrapper = ({ children }: { children: ReactNode }) => (
      <DataProvider api={api} queryClient={queryClient} token={DEVICE_TOKEN}>
        {children}
      </DataProvider>
    );
    const hook = renderHook(() => useRecapBlocks(), { wrapper });
    expect(hook.result.current.isPending).toBe(true);
    expect(hook.result.current.fetchStatus).not.toBe('fetching');
    await waitFor(() => expect(hook.result.current.data).toBeDefined());
  });

  it('clauses() round-trips a real block line from the fixture', async () => {
    hub = await spawnHub(await freePort());
    const api = createApi({ baseUrl: hub.url, token: DEVICE_TOKEN });
    const page = await api.recapBlocks({}, undefined, 1);
    const block = page.blocks[0];
    if (block === undefined) throw new Error('the fixture has no blocks');
    const parts = clauses(block.line);
    expect(parts.map((p) => p.text).join('')).toBe(block.line.text);
    expect(parts.some((p) => p.receipts.length > 0)).toBe(true);
  });
});
