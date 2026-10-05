// Test harness: a real mock hub per test, and components rendered inside the data layer.
//
// The hub runs as a child process through stream L's `apps/ui/tests/hub-process.ts` (happy-dom
// replaces globals, such as URL, that an in-process hub needs), on a free port. That helper uses
// Node APIs, which the app's tsconfig (browser types only) does not know, so it is imported by a
// path TypeScript does not follow.

import type { QueryClient } from '@tanstack/react-query';
import { cleanup, configure, render } from '@testing-library/react';
import type { ReactNode } from 'react';
import { vi } from 'vitest';
import { createApi, createQueryClient, DataProvider, type Api } from '../../data/index.ts';
import { ProjectsNavProvider, type ProjectsNav } from '../nav.tsx';
import { RecapTzProvider } from '../recap-tz.tsx';

// Several hubs start at once when the files run in parallel; give data time to arrive.
const PATIENCE_MS = 5_000;
configure({ asyncUtilTimeout: PATIENCE_MS });

/** `vi.waitFor` with the same patience as `findBy…`. */
export function eventually<T>(check: () => T | Promise<T>): Promise<T> {
  return vi.waitFor(check, { timeout: PATIENCE_MS, interval: 50 });
}

export const DEVICE_TOKEN = 'dev-device-token';
export const AGENT_TOKEN = 'dev-agent-token';

export interface Hub {
  url: string;
  close(): Promise<void>;
}

interface HubProcessModule {
  freePort(): Promise<number>;
  spawnHub(port: number, env?: Record<string, string>): Promise<Hub>;
}

const HUB_PROCESS = '../../../tests/hub-process.ts';

/** A fresh mock hub; `env` adds `PITCREW_MOCK_*` settings to its environment. */
export async function startHub(env: Record<string, string> = {}): Promise<Hub> {
  const { freePort, spawnHub } = (await import(/* @vite-ignore */ HUB_PROCESS)) as HubProcessModule;
  return spawnHub(await freePort(), env);
}

const clients = new Set<QueryClient>();

/** Unmounts everything, lets in-flight requests stop, then stops the hub. */
export async function stopHub(hub: Hub): Promise<void> {
  cleanup();
  for (const client of clients) {
    await client.cancelQueries();
    client.clear();
  }
  clients.clear();
  await new Promise((done) => setTimeout(done, 25));
  await hub.close();
}

/**
 * Renders inside `DataProvider`. The stream always uses the device token (it is device-only);
 * `token` sets the one the API calls carry, so a test can act as the agent @writer. `fetch`
 * stands between the API client and the hub, to slow or rewrite answers. Recap days use `tz=0`,
 * the only offset the mock hub has days for, whatever the machine's own time zone.
 */
export function renderWithHub(
  ui: ReactNode,
  hub: Hub,
  options: { token?: string; nav?: ProjectsNav; fetch?: typeof fetch } = {},
) {
  const api = createApi({
    baseUrl: hub.url,
    token: options.token ?? DEVICE_TOKEN,
    ...(options.fetch === undefined ? {} : { fetch: options.fetch }),
  });
  const queryClient = createQueryClient();
  clients.add(queryClient);
  const result = render(
    <DataProvider api={api} queryClient={queryClient} token={DEVICE_TOKEN}>
      <RecapTzProvider tz={0}>
        <ProjectsNavProvider value={options.nav ?? {}}>{ui}</ProjectsNavProvider>
      </RecapTzProvider>
    </DataProvider>,
  );
  return { ...result, api, queryClient };
}

/** Another client of the same hub: someone else's desktop, or an agent. */
export function otherClient(hub: Hub, token: string = DEVICE_TOKEN): Api {
  return createApi({ baseUrl: hub.url, token });
}

/** Ids from `crates/fixtures/data/demo-workspace.json`. */
export const demo = {
  sam: '01JB000000000000000MEM0001',
  writer: '01JB000000000000000MEM0002',
  reviewer: '01JB000000000000000MEM0004',
  paper: '01JB000000000000000PRJ0001',
  tooling: '01JB000000000000000PRJ0002',
  submission: '01JB000000000000000WST0001',
  seedRuns: '01JB000000000000000WST0002',
  parsers: '01JB000000000000000WST0003',
  pap1: '01JB000000000000000TSK0001',
  pap2: '01JB000000000000000TSK0002',
  pap3: '01JB000000000000000TSK0003',
  pap4: '01JB000000000000000TSK0004',
  pap5: '01JB000000000000000TSK0005',
  pap6: '01JB000000000000000TSK0006',
  pap7: '01JB000000000000000TSK0007',
  ses1: '01JB000000000000000SES0001',
  ask1: '01JB000000000000000ASK0001',
  ask2: '01JB000000000000000ASK0002',
  ask3: '01JB000000000000000ASK0003',
} as const;
