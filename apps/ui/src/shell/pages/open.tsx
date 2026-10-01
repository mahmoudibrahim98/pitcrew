// `/` opens a workspace: in a browser the hub's one workspace; in the desktop app the one last
// opened, or else the first the gateway can reach. `/w/$ws` opens its stored layout where it was
// last left.

import { useParams, useRouter } from '@tanstack/react-router';
import { useEffect } from 'react';
import {
  useConnection,
  useGatewayWorkspaces,
  useWorkspace,
  useWorkstreams,
  type GatewayWorkspace,
  type WorkspaceList,
} from '../../data/index.ts';
import { LAYOUTS, useWorkspaceId } from '../layout.ts';
import { paths } from '../paths.ts';
import { lastPath, storedLayout, useShell } from '../store.ts';
import { NotFoundPage } from './not-found.tsx';
import { LOADING, Page } from './page.tsx';
import { StatusScreen } from './unavailable.tsx';

/** Replaces the location with `href` (a path with its search). */
function Redirect({ href }: { href: string }) {
  const router = useRouter();
  useEffect(() => {
    void router.navigate({ href, replace: true });
  }, [router, href]);
  return null;
}

export function OpenWorkspace() {
  const desktop = useGatewayWorkspaces();
  return desktop === null ? <OpenHubWorkspace /> : <OpenDesktopWorkspace workspaces={desktop} />;
}

function OpenHubWorkspace() {
  const workspace = useWorkspace();
  const { problem } = useConnection();
  if (workspace.data !== undefined) {
    return <Redirect href={paths.workspace(workspace.data.workspace.id)} />;
  }
  let message = 'Connecting to the PitCrew hub…';
  if (problem === 'unauthorized') message = 'The hub did not accept this app’s token.';
  else if (problem === 'unreachable') message = 'Cannot reach the PitCrew hub. Retrying…';
  return <StatusScreen>{message}</StatusScreen>;
}

/** The last workspace opened, if the gateway still lists it; else the first ready one; else the first. */
export function pickWorkspace(list: readonly GatewayWorkspace[], last: string | null): GatewayWorkspace | undefined {
  return list.find((w) => w.id === last) ?? list.find((w) => w.state === 'ready') ?? list[0];
}

function OpenDesktopWorkspace({ workspaces }: { workspaces: WorkspaceList }) {
  const last = useShell((s) => s.lastWorkspace);
  const { list, error } = workspaces;
  const pick = list === undefined ? undefined : pickWorkspace(list, last);
  if (pick !== undefined) return <Redirect href={paths.workspace(pick.id)} />;
  if (list !== undefined) return <StatusScreen>No workspaces yet.</StatusScreen>;
  if (error !== undefined) return <StatusScreen>Could not list the workspaces: {error}</StatusScreen>;
  return <StatusScreen>Loading workspaces…</StatusScreen>;
}

/** `workstreams/$workstream`: for callers that know a workstream but not its project. */
export function OpenWorkstream() {
  const ws = useWorkspaceId();
  const { workstream: id }: { workstream?: string } = useParams({ strict: false });
  const workstreams = useWorkstreams().data;
  if (workstreams === undefined) return <Page title={LOADING} placeholder={false} />;
  const workstream = workstreams.find((w) => w.id === id);
  if (workstream === undefined) return <NotFoundPage />;
  return <Redirect href={paths.workstream(ws, workstream.project, workstream.id)} />;
}

export function OpenLayout() {
  const ws = useWorkspaceId();
  const layout = storedLayout(ws);
  return <Redirect href={lastPath(ws, layout) ?? paths.under(ws, LAYOUTS[layout].home)} />;
}
