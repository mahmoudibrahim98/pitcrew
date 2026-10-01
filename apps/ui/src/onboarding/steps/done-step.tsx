// The last step: a short summary, then Home.

import { useRouter } from '@tanstack/react-router';
import { Button } from '../../design/index.ts';
import { paths, useWorkspaceId } from '../../shell/index.ts';
import { useWizard } from '../wizard-context.tsx';

export function DoneStep() {
  const { state } = useWizard();
  const ws = useWorkspaceId();
  const router = useRouter();

  const setup = state.setupResult;
  const projectCount = state.createResult?.projects.length ?? 0;
  const importedCount = state.importResult?.imported ?? 0;

  return (
    <div>
      <p className="text-sm text-ink-2">
        {setup === undefined
          ? 'Your workspace is ready.'
          : `“${setup.workspace.name}” is ready, with you as ${setup.me.name} (${setup.me.handle}) on ${state.setup.machineName.trim()}.`}
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
          Go to Home
        </Button>
      </div>
    </div>
  );
}
