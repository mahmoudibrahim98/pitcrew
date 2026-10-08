// The last step: a short summary, the first things to do, then Home.

import { useRouter } from '@tanstack/react-router';
import { use } from 'react';
import { Button } from '../../design/index.ts';
import { paths, useWorkspaceId } from '../../shell/index.ts';
import { useShell } from '../../shell/store.ts';
import { RegistryContext } from '../../shell/context.ts';
import { useWizard } from '../wizard-context.tsx';

/** The "+ New" entries offered as first steps (when registered and enabled), with their labels here. */
const FIRST_STEPS: Partial<Record<string, string>> = { session: 'Start a session', task: 'Create a task' };

export function DoneStep() {
  const { state } = useWizard();
  const ws = useWorkspaceId();
  const router = useRouter();
  const registry = use(RegistryContext);
  // Each opens its "+ New" dialog on the page it belongs to. This step is gone by then, so the
  // dialog gives focus back to that page, as the palette does after it navigates.
  const start = (id: string) => {
    void router
      .navigate({ href: id === 'session' ? paths.console(ws) : paths.home(ws), replace: true })
      .then(() => useShell.getState().setCreating(id, document.getElementById('main')));
  };
  // Inviting someone waits for invites: the hub has no way to add a person yet.
  const firstSteps = (registry?.create ?? []).flatMap((entry) => {
    const label = entry.disabled === undefined ? FIRST_STEPS[entry.id] : undefined;
    return label === undefined ? [] : [{ id: entry.id, label }];
  });

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
        <p className="mt-1 text-xs text-ink-2">Start an agent session, or create a task to work on.</p>
        <div className="mt-3 flex flex-wrap gap-2">
          {firstSteps.map((step) => (
            <Button key={step.id} onClick={() => start(step.id)}>
              {step.label}
            </Button>
          ))}
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
