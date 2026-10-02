// The first run (`POST /v1/setup`, api-v1.md "The first run"). Unlike other writes, setup changes
// what no event carries: the workspace's name and `setup_needed`. So it updates the cache itself,
// at once, before anything navigates on the old answer (the shell sends a workspace that needs
// setup to the first-run wizard, and a stale `setup_needed: true` would send it straight back).

import { useMutation, useQueryClient, type QueryClient } from '@tanstack/react-query';
import { useCallback } from 'react';
import type { Api } from './api.ts';
import { ApiError } from './errors.ts';
import { keys } from './keys.ts';
import { useApi } from './provider.tsx';
import type { Member, Setup, SetupResult, WorkspaceInfo } from './types.ts';

/**
 * A `409 conflict` from setup. The hub says no for two reasons, and the workspace, read again,
 * says which: `alreadySetUp` (someone finished setup first: go Home) or the handle is taken.
 */
export class SetupConflict extends ApiError {
  readonly alreadySetUp: boolean;

  constructor(error: ApiError, alreadySetUp: boolean) {
    super(error.code, error.message, error.status);
    this.name = 'SetupConflict';
    this.alreadySetUp = alreadySetUp;
  }
}

/** The keys setup changes: the workspace, `me`, members and machines. */
const REFRESHED = [keys.workspace, keys.me, keys.members, keys.machines] as const;

/**
 * Sets the workspace up through `api` and refreshes `queryClient`, the cache of the same
 * workspace: `setup_needed` turns `false` and `me` is the new person at once, then the four keys
 * refetch. A `409` rejects with a `SetupConflict`; anything else with the `ApiError`.
 */
export async function setUp(api: Api, queryClient: QueryClient, setup: Setup): Promise<SetupResult> {
  let result: SetupResult;
  try {
    result = await api.setup(setup);
  } catch (error) {
    if (error instanceof ApiError && error.code === 'conflict') throw await conflict(api, queryClient, error);
    throw error;
  }
  queryClient.setQueryData<WorkspaceInfo>(keys.workspace, (old) =>
    old === undefined ? old : { ...old, workspace: result.workspace, setup_needed: false },
  );
  queryClient.setQueryData<Member>(keys.me, result.me);
  // Not awaited: the cache is already right, and a refetch held up by a slow stream must not
  // hold up the wizard. Invalidating also cancels a fetch from before setup still in flight.
  for (const queryKey of REFRESHED) void queryClient.invalidateQueries({ queryKey });
  return result;
}

async function conflict(api: Api, queryClient: QueryClient, error: ApiError): Promise<SetupConflict> {
  let alreadySetUp = false;
  try {
    const info = await api.workspace();
    queryClient.setQueryData<WorkspaceInfo>(keys.workspace, info);
    alreadySetUp = info.setup_needed !== true;
  } catch {
    // Cannot tell: the hub's own message says which.
  }
  // Someone set it up, or holds the handle: either way the same four keys may have changed.
  for (const queryKey of REFRESHED) void queryClient.invalidateQueries({ queryKey });
  return new SetupConflict(error, alreadySetUp);
}

/** `setUp` bound to this scope's API and cache (a stable function). */
export function useSetUp(): (setup: Setup) => Promise<SetupResult> {
  const api = useApi();
  const queryClient = useQueryClient();
  return useCallback((setup: Setup) => setUp(api, queryClient, setup), [api, queryClient]);
}

/** The setup mutation: `POST /v1/setup` for this scope's workspace, refreshing its cache. */
export function useSetup() {
  const run = useSetUp();
  return useMutation({ mutationFn: run });
}
