// Step 5: sign in to each agent CLI on the machine with the CLI's own login (`sign-in-panel.tsx`).
// The accounts read stay in the wizard's state, so coming back to the step shows them at once.

import { useOnboardingApi } from '../api-context.tsx';
import { SignInPanel } from '../sign-in-panel.tsx';
import { StepFooter } from '../step-footer.tsx';
import { useWizard } from '../wizard-context.tsx';

export function SignInStep() {
  const { state, patch, next, skip } = useWizard();
  const api = useOnboardingApi();
  return (
    <form
      onSubmit={(e) => {
        e.preventDefault();
        next();
      }}
    >
      <SignInPanel
        api={api}
        target={state.primaryMachine}
        machineLabel={state.setup.machineName.trim() || undefined}
        cached={state.accountsStatus === 'done' ? state.accounts : undefined}
        onAccounts={(accounts) => patch({ accounts, accountsStatus: 'done' })}
      />
      <StepFooter nextLabel="Continue" onSkip={skip} skipLabel="Skip for now" />
    </form>
  );
}
