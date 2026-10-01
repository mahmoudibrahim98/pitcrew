// What shows instead of a page when there is no workspace data to show: a full-screen status
// line (opening, listing workspaces), or, in the desktop app, why the gateway cannot reach a
// workspace. Never a spinner: a workspace that is unreachable or needs pairing says so.

import type { ReactNode } from 'react';
import type { GatewayWorkspace } from '../../data/index.ts';
import { Button } from '../../design/index.ts';

export function StatusScreen({ children, onRetry }: { children: ReactNode; onRetry?: () => void }) {
  return (
    <main className="grid min-h-dvh place-items-center bg-bg px-6 text-ink">
      <div className="flex flex-col items-center gap-3">
        <p role="status" className="text-sm text-ink-2">
          {children}
        </p>
        {onRetry !== undefined && <Button onClick={onRetry}>Retry</Button>}
      </div>
    </main>
  );
}

/** The desktop app could not list its workspaces. It keeps trying; Retry tries now. */
export function WorkspacesFailed({ message, onRetry }: { message: string; onRetry: () => void }) {
  return (
    <StatusScreen onRetry={onRetry}>
      Could not list the workspaces ({message}). PitCrew keeps trying.
    </StatusScreen>
  );
}

/** The workspace's state, in a few words, for the switcher and the top bar. */
export const WORKSPACE_STATE_LABEL: Record<GatewayWorkspace['state'], string> = {
  connecting: 'Connecting',
  ready: 'Ready',
  unreachable: 'Unreachable',
  needs_pairing: 'Needs pairing',
};

/** In the frame's main area, for a workspace the gateway cannot reach or has no token for. */
export function WorkspaceUnavailable({ workspace }: { workspace: GatewayWorkspace }) {
  const pairing = workspace.state === 'needs_pairing';
  return (
    <div
      className="mx-auto flex max-w-md flex-col items-start gap-2 px-6 py-16"
      data-testid="workspace-unavailable"
      data-state={workspace.state}
    >
      <p className="font-mono text-xs text-ink-2">{WORKSPACE_STATE_LABEL[workspace.state]}</p>
      <h1 className="text-xl font-semibold">
        {pairing ? `${workspace.name} needs pairing` : `Cannot reach ${workspace.name}`}
      </h1>
      <p className="text-sm text-ink-2">
        {pairing
          ? 'This app has no valid token for this workspace. Once it is paired again, its pages come back here.'
          : 'The desktop app cannot reach this workspace’s daemon. Its pages come back here as soon as it can.'}
      </p>
      {workspace.detail !== undefined && workspace.detail !== '' && (
        <p className="text-sm text-ink-2" data-testid="workspace-detail">
          {workspace.detail}
        </p>
      )}
    </div>
  );
}
