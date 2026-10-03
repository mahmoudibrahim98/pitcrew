// "Remove workspace…" (remote workspaces, desktop only): asks first, and offers to stop PitCrew on
// the remote too, which cancels its SLURM job (`gateway_workspace_remove`, desktop-gateway.md).
// Then it opens another workspace, or `/`.

import { useRouter } from '@tanstack/react-router';
import { useId, useState } from 'react';
import { useGatewayWorkspaces, type GatewayWorkspace, type RemoteGateway } from '../data/index.ts';
import { Button, Dialog, DialogContent, DialogFooter } from '../design/index.ts';
import { WorkspaceName } from './workspace-name.tsx';
import { paths } from './paths.ts';
import { useShell } from './store.ts';

function messageOf(error: unknown): string {
  return error instanceof Error ? error.message : 'The gateway could not remove it.';
}

export function RemoveWorkspaceDialog({
  workspace,
  remote,
  onClose,
  returnFocus,
}: {
  workspace: GatewayWorkspace;
  remote: RemoteGateway;
  onClose(): void;
  /** Where focus goes once the dialog has gone (it has no trigger of its own). */
  returnFocus(): void;
}) {
  const router = useRouter();
  const list = useGatewayWorkspaces()?.list ?? [];
  const [stopHelper, setStopHelper] = useState(false);
  const [busy, setBusy] = useState(false);
  const [error, setError] = useState<string | null>(null);
  const stopId = useId();

  async function remove() {
    setBusy(true);
    setError(null);
    try {
      await remote.workspaceRemove(workspace.id, stopHelper);
    } catch (cause) {
      setError(messageOf(cause));
      setBusy(false);
      return;
    }
    // Somewhere else to be: the list may not have dropped it yet, so pick past it.
    const others = list.filter((w) => w.id !== workspace.id);
    const next = others.find((w) => w.state === 'ready') ?? others[0];
    useShell.setState({ lastWorkspace: next?.id ?? null });
    onClose();
    void router.navigate({ href: next === undefined ? '/' : paths.workspace(next.id) });
  }

  return (
    <Dialog open onOpenChange={(open) => !open && !busy && onClose()}>
      <DialogContent
        title={<>Remove <WorkspaceName workspace={workspace} suffix="?" /></>}
        description="This app forgets the workspace and its key. Its data stays on the remote."
        onCloseAutoFocus={(event) => {
          event.preventDefault();
          returnFocus();
        }}
      >
        <form
          onSubmit={(event) => {
            event.preventDefault();
            void remove();
          }}
        >
          <div className="flex flex-col gap-3 px-4 py-4 text-sm">
            <label htmlFor={stopId} className="flex items-start gap-2 text-ink">
              <input
                id={stopId}
                type="checkbox"
                checked={stopHelper}
                onChange={(event) => setStopHelper(event.target.checked)}
                className="mt-0.5"
              />
              <span>Also stop PitCrew on the remote (cancels its SLURM job)</span>
            </label>
            {error !== null && (
              <p role="alert" className="text-risk">
                {error}
              </p>
            )}
          </div>
          <DialogFooter>
            <Button onClick={onClose} disabled={busy}>
              Cancel
            </Button>
            <Button type="submit" variant="primary" disabled={busy}>
              {busy ? 'Removing…' : 'Remove'}
            </Button>
          </DialogFooter>
        </form>
      </DialogContent>
    </Dialog>
  );
}
