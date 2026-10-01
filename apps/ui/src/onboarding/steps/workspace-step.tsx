// Step 2 (first run: name the workspace and its primary machine) and the add-a-machine wizard's
// own first step (just the machine: it already has a workspace and a name). `setupWorkspace` is
// the only call this step makes; the real hub has no such route yet (see README.md).

import { useId, useState } from 'react';
import { useOnboardingApi } from '../api-context.tsx';
import { MachineTargetPicker } from '../machine-target-picker.tsx';
import { StepFooter } from '../step-footer.tsx';
import { useWizard } from '../wizard-context.tsx';

export function WorkspaceStep() {
  const { mode, state, patch, next } = useWizard();
  const api = useOnboardingApi();
  const [busy, setBusy] = useState(false);
  const [error, setError] = useState<string | null>(null);
  const nameId = useId();
  const firstRun = mode === 'first-run';

  async function submit() {
    setError(null);
    if (firstRun && state.workspaceName.trim() === '') {
      setError('Give the workspace a name.');
      return;
    }
    setBusy(true);
    try {
      if (firstRun) {
        await api.setupWorkspace({ name: state.workspaceName, primaryMachine: state.primaryMachine });
      }
      next();
    } catch (cause) {
      setError(cause instanceof Error ? cause.message : 'Could not save the workspace.');
    } finally {
      setBusy(false);
    }
  }

  return (
    <form
      onSubmit={(e) => {
        e.preventDefault();
        void submit();
      }}
    >
      {firstRun && (
        <div className="flex flex-col gap-1.5">
          <label htmlFor={nameId} className="text-sm font-medium text-ink">
            Workspace name
          </label>
          <input
            id={nameId}
            type="text"
            aria-invalid={error !== null}
            value={state.workspaceName}
            onChange={(e) => patch({ workspaceName: e.target.value })}
            placeholder="My team"
            className="h-8 rounded-sm border border-line-2 bg-card px-2.5 text-sm text-ink outline-none focus-visible:border-accent"
          />
        </div>
      )}

      <div className={firstRun ? 'mt-4' : undefined}>
        <MachineTargetPicker
          value={state.primaryMachine}
          onChange={(primaryMachine) => patch({ primaryMachine })}
          label={firstRun ? 'Primary machine' : 'Machine to add'}
        />
      </div>

      {error !== null && (
        <p role="alert" className="mt-3 text-sm text-risk">
          {error}
        </p>
      )}

      <StepFooter nextLabel="Continue" busy={busy} />
    </form>
  );
}
