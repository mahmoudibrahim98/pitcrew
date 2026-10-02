// Step 2: set the hub up (`POST /v1/setup`): the workspace's name, your name and handle, and this
// machine's name. Once the hub has taken it, the step only shows what was set: going Back to it
// never sends it again. A `409` because the workspace was set up meanwhile goes Home.

import { useRouter } from '@tanstack/react-router';
import { paths, useWorkspaceId } from '../../shell/index.ts';
import { useOnboardingApi } from '../api-context.tsx';
import { SetupForm } from '../setup-form.tsx';
import { StepFooter } from '../step-footer.tsx';
import { useWizard } from '../wizard-context.tsx';

export function WorkspaceStep() {
  const { state, patch, next, machineLabel } = useWizard();
  const api = useOnboardingApi();
  const router = useRouter();
  const ws = useWorkspaceId();
  const done = state.setupResult;

  if (done !== undefined) {
    return (
      <form
        onSubmit={(event) => {
          event.preventDefault();
          next();
        }}
      >
        <p className="text-sm text-ink-2">
          “{done.workspace.name}” is set up, with you as {done.me.name} ({done.me.handle}).
        </p>
        <StepFooter />
      </form>
    );
  }

  return (
    <SetupForm
      values={state.setup}
      handleEdited={state.handleEdited}
      onChange={(setup, handleEdited) => patch({ setup, handleEdited })}
      submit={(input) => api.setupWorkspace(input)}
      onDone={(setupResult) => {
        patch({ setupResult });
        next();
      }}
      onAlreadySetUp={() => void router.navigate({ href: paths.home(ws), replace: true })}
      machineLabel={machineLabel}
      footer={(busy) => <StepFooter nextLabel="Continue" busy={busy} />}
    />
  );
}
