// @vitest-environment happy-dom
// The setup mutation (`POST /v1/setup`) through the desktop gateway, over a mocked `invoke`: the
// request it sends, and the cache it leaves — the workspace no longer needs setup, `me` is the new
// person, and members and machines refetch — and the two kinds of `409`.

import { QueryClient, QueryObserver } from '@tanstack/react-query';
import { afterEach, beforeEach, describe, expect, it } from 'vitest';
import { createApi } from '../api.ts';
import { ApiError } from '../errors.ts';
import { gatewayTransport } from '../gateway.ts';
import { keys } from '../keys.ts';
import { createQueryClient } from '../provider.tsx';
import { setUp, SetupConflict } from '../setup.ts';
import type { Member, Setup, WorkspaceInfo } from '../types.ts';
import { FakeDesktop, freshDaemon, json, ME_ID } from './fake-desktop.ts';

const WS = '01JB000000000000000WSPFRSH';
const SETUP: Setup = {
  workspace_name: 'Demo Lab',
  person: { name: 'Sam Rivera', handle: '@sam' },
  machine_name: 'This laptop',
};

let desktop: FakeDesktop;
let queryClient: QueryClient;

beforeEach(() => {
  desktop = new FakeDesktop({ workspaces: [{ id: WS, name: 'Demo Lab', kind: 'local', state: 'ready' }] }).install();
  queryClient = createQueryClient();
});

afterEach(() => {
  queryClient.clear();
  desktop.uninstall();
});

function api() {
  return createApi({ transport: gatewayTransport(WS) });
}

/** Keeps the four keys observed, as the shell's pages do, so invalidation refetches them. */
function observe(client: ReturnType<typeof api>): () => void {
  const observers = [
    new QueryObserver(queryClient, { queryKey: keys.workspace, queryFn: () => client.workspace() }),
    new QueryObserver(queryClient, { queryKey: keys.me, queryFn: () => client.me(), retry: false }),
    new QueryObserver(queryClient, { queryKey: keys.members, queryFn: () => client.members() }),
    new QueryObserver(queryClient, { queryKey: keys.machines, queryFn: () => client.machines() }),
  ];
  const stops = observers.map((o) => o.subscribe(() => {}));
  return () => stops.forEach((stop) => stop());
}

async function settled(): Promise<void> {
  await new Promise((done) => setTimeout(done, 0));
  while (queryClient.isFetching() > 0) await new Promise((done) => setTimeout(done, 5));
}

describe('setUp', () => {
  it('sends the body once, and leaves the cache set up', async () => {
    const fresh = freshDaemon(WS);
    desktop.daemons.set(WS, fresh.daemon);
    const client = api();
    const stop = observe(client);
    await settled();
    expect(queryClient.getQueryData<WorkspaceInfo>(keys.workspace)?.setup_needed).toBe(true);
    expect(queryClient.getQueryState(keys.me)?.status).toBe('error');

    const result = await setUp(client, queryClient, SETUP);
    expect(result.me).toEqual({ id: ME_ID, kind: 'human', handle: '@sam', name: 'Sam Rivera' });
    // At once, before any refetch: the shell must not see `setup_needed` again.
    expect(queryClient.getQueryData<WorkspaceInfo>(keys.workspace)?.setup_needed).toBe(false);
    expect(queryClient.getQueryData<WorkspaceInfo>(keys.workspace)?.workspace.name).toBe('Demo Lab');
    expect(queryClient.getQueryData<Member>(keys.me)?.name).toBe('Sam Rivera');

    await settled();
    expect(queryClient.getQueryData<Member[]>(keys.members)?.map((m) => m.handle)).toEqual(['@sam']);
    expect(queryClient.getQueryData<unknown[]>(keys.machines)).toHaveLength(1);
    expect(queryClient.getQueryData<WorkspaceInfo>(keys.workspace)?.setup_needed).toBeUndefined();

    const posts = desktop
      .commands('gateway_request')
      .map((a) => a.req as { method: string; path: string; body?: string })
      .filter((r) => r.method === 'POST');
    expect(posts).toEqual([{ workspace: WS, method: 'POST', path: '/v1/setup', body: JSON.stringify(SETUP) }]);
    stop();
  });

  it('says the workspace was already set up when a 409 comes from a set-up hub', async () => {
    const fresh = freshDaemon(WS);
    desktop.daemons.set(WS, fresh.daemon);
    const client = api();
    const stop = observe(client);
    await settled();
    // Someone else finishes setup first.
    await client.setup({ ...SETUP, person: { name: 'Alex Kim', handle: '@alex' } });

    const error = await setUp(client, queryClient, SETUP).catch((e: unknown) => e);
    expect(error).toBeInstanceOf(SetupConflict);
    expect(error).toBeInstanceOf(ApiError);
    expect((error as SetupConflict).alreadySetUp).toBe(true);
    expect(queryClient.getQueryData<WorkspaceInfo>(keys.workspace)?.setup_needed).toBeUndefined();
    stop();
  });

  it('says the handle is taken when the hub still needs setup', async () => {
    desktop.daemons.set(WS, (req) =>
      req.method === 'POST'
        ? json(409, { code: 'conflict', message: 'The handle @sam is already taken.' })
        : json(200, { workspace: { id: WS, name: '' }, rev: 1, setup_needed: true }),
    );
    const error = await setUp(api(), queryClient, SETUP).catch((e: unknown) => e);
    expect(error).toMatchObject({ alreadySetUp: false, code: 'conflict', message: 'The handle @sam is already taken.' });
  });

  it('passes a 400 on as it is, and leaves the cache alone', async () => {
    desktop.daemons.set(WS, (req) =>
      req.method === 'POST'
        ? json(400, { code: 'invalid', message: 'person.handle must be "@" followed by 1 to 32 of a-z, 0-9, "_" or "-".' })
        : json(200, { workspace: { id: WS, name: '' }, rev: 1, setup_needed: true }),
    );
    const client = api();
    const stop = observe(client);
    await settled();
    const error = await setUp(client, queryClient, SETUP).catch((e: unknown) => e);
    expect(error).not.toBeInstanceOf(SetupConflict);
    expect(error).toMatchObject({ code: 'invalid', status: 400 });
    expect(queryClient.getQueryData<WorkspaceInfo>(keys.workspace)?.setup_needed).toBe(true);
    stop();
  });
});
