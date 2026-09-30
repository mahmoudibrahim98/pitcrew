// `/` opens the workspace (for now the hub's one workspace); `/w/$ws` opens its stored layout
// where it was last left.

import { useRouter } from '@tanstack/react-router';
import { useEffect } from 'react';
import { useConnection, useWorkspace } from '../../data/index.ts';
import { LAYOUTS, useWorkspaceId } from '../layout.ts';
import { paths } from '../paths.ts';
import { lastPath, storedLayout } from '../store.ts';

/** Replaces the location with `href` (a path with its search). */
function Redirect({ href }: { href: string }) {
  const router = useRouter();
  useEffect(() => {
    void router.navigate({ href, replace: true });
  }, [router, href]);
  return null;
}

export function OpenWorkspace() {
  const workspace = useWorkspace();
  const { problem } = useConnection();
  if (workspace.data !== undefined) {
    return <Redirect href={paths.workspace(workspace.data.workspace.id)} />;
  }
  let message = 'Connecting to the PitCrew hub…';
  if (problem === 'unauthorized') message = 'The hub did not accept this app’s token.';
  else if (problem === 'unreachable') message = 'Cannot reach the PitCrew hub. Retrying…';
  return (
    <main className="grid min-h-dvh place-items-center bg-bg text-ink">
      <p role="status" className="text-sm text-ink-2">
        {message}
      </p>
    </main>
  );
}

export function OpenLayout() {
  const ws = useWorkspaceId();
  const layout = storedLayout(ws);
  return <Redirect href={lastPath(ws, layout) ?? paths.under(ws, LAYOUTS[layout].home)} />;
}
