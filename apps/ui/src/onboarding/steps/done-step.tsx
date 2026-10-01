// Step 12 (first run) / the add-a-machine wizard's last step: a short summary, then Home.

import { useRouter } from '@tanstack/react-router';
import { Button } from '../../design/index.ts';
import { paths, useWorkspaceId } from '../../shell/index.ts';
import { machineTargetLabel } from '../api.ts';
import { useWizard } from '../wizard-context.tsx';

export function DoneStep() {
  const { mode, state } = useWizard();
  const ws = useWorkspaceId();
  const router = useRouter();
  const firstRun = mode === 'first-run';

  const projectCount = state.createResult?.projects.length ?? 0;
  const importedCount = state.importResult?.imported ?? 0;

  return (
    <div>
      <p className="text-sm text-ink-2">
        {firstRun
          ? `"${state.workspaceName || 'Your workspace'}" is ready on ${machineTargetLabel(state.primaryMachine)}.`
          : `${machineTargetLabel(state.primaryMachine)} is connected.`}
      </p>
      <ul className="mt-3 flex flex-col gap-1 text-sm text-ink-2">
        {projectCount > 0 && (
          <li>
            Created {projectCount} {projectCount === 1 ? 'project' : 'projects'}.
          </li>
        )}
        {importedCount > 0 && (
          <li>
            Imported {importedCount} {importedCount === 1 ? 'session' : 'sessions'}.
          </li>
        )}
        {state.hooksInstalled && <li>Hooks installed.</li>}
        {state.accounts.some((a) => a.signedIn) && <li>Signed in to your agents.</li>}
      </ul>
      <div className="mt-6">
        <Button variant="primary" onClick={() => void router.navigate({ href: paths.home(ws) })}>
          {firstRun ? 'Go to Home' : 'Done'}
        </Button>
      </div>
    </div>
  );
}
