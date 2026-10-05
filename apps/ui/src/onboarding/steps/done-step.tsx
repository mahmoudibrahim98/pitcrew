// The last step: a short summary, then Home.

import { useRouter } from '@tanstack/react-router';
import { use } from 'react';
import { Button } from '../../design/index.ts';
import { paths, useWorkspaceId } from '../../shell/index.ts';
import { useShell } from '../../shell/store.ts';
import { RegistryContext } from '../../shell/context.ts';
import { useWizard } from '../wizard-context.tsx';

export function DoneStep() {
  const { state } = useWizard();
  const ws = useWorkspaceId();
  const router = useRouter();
  const registry = use(RegistryContext);
  const start = (id: string) => {
    void router.navigate({ href: id === 'session' ? paths.console(ws) : paths.home(ws), replace: true }).then(() => useShell.getState().setCreating(id));
  };

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
      <div className="mt-6 rounded-md border border-line bg-sidebar p-4">
        <h2 className="text-sm font-medium text-ink">What would you like to do first?</h2>
        <p className="mt-1 text-xs text-ink-2">Start a session, create a task, or invite someone to work with you.</p>
        <div className="mt-3 flex flex-wrap gap-2">
          {registry?.create.filter((entry) => ['session', 'task', 'human', 'member'].includes(entry.id) && entry.disabled === undefined).map((entry) => <Button key={entry.id} onClick={() => start(entry.id)}>{entry.id === 'session' ? 'Start a session' : entry.id === 'task' ? 'Create a task' : 'Invite someone'}</Button>)}
          <Button onClick={() => void router.navigate({ href: paths.console(ws), replace: true })}>Open agent sessions</Button>
        </div>
      </div>
      <div className="mt-4">
        {/* Replacing the wizard: Back from Home is not the finished first run. */}
        <Button variant="primary" onClick={() => void router.navigate({ href: paths.home(ws), replace: true })}>
          Go to Home
        </Button>
      </div>
    </div>
  );
}
