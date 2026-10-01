// The stepper and content pane shared by both wizards. The stepper is a Radix `Tabs.Root` in
// vertical orientation, which gives arrow-key navigation and roving tabindex for free; only steps
// at or before `furthest` are enabled, so the keyboard and the pointer agree on what is reachable.

import { Tabs } from 'radix-ui';
import type { ComponentType } from 'react';
import { CheckIcon } from '../design/index.ts';
import { cx } from '../lib/cx.ts';
import type { StepId } from './steps.ts';
import { useWizard } from './wizard-context.tsx';
import { CreateStep } from './steps/create-step.tsx';
import { DoneStep } from './steps/done-step.tsx';
import { HooksStep } from './steps/hooks-step.tsx';
import { ImportStep } from './steps/import-step.tsx';
import { InstallHelperStep } from './steps/install-helper-step.tsx';
import { IntegrationsStep } from './steps/integrations-step.tsx';
import { MachineCheckStep } from './steps/machine-check-step.tsx';
import { SafetyStep } from './steps/safety-step.tsx';
import { ScanStep } from './steps/scan-step.tsx';
import { SignInStep } from './steps/sign-in-step.tsx';
import { WelcomeStep } from './steps/welcome-step.tsx';
import { WorkspaceStep } from './steps/workspace-step.tsx';

const STEP_COMPONENTS: Record<StepId, ComponentType> = {
  welcome: WelcomeStep,
  workspace: WorkspaceStep,
  'machine-check': MachineCheckStep,
  'install-helper': InstallHelperStep,
  'sign-in': SignInStep,
  integrations: IntegrationsStep,
  scan: ScanStep,
  create: CreateStep,
  import: ImportStep,
  hooks: HooksStep,
  safety: SafetyStep,
  done: DoneStep,
};

export function WizardShell() {
  const { steps, stepIndex, furthest, currentStep, goTo } = useWizard();
  const Step = STEP_COMPONENTS[currentStep.id];

  return (
    <div className="mx-auto flex min-h-dvh max-w-4xl gap-10 px-8 py-10">
      <Tabs.Root
        orientation="vertical"
        value={currentStep.id}
        onValueChange={(value) => {
          const index = steps.findIndex((s) => s.id === value);
          if (index !== -1) goTo(index);
        }}
        className="flex w-full gap-10"
      >
        <Tabs.List aria-label="Onboarding steps" className="flex w-48 shrink-0 flex-col gap-0.5">
          {steps.map((step, index) => {
            const reached = index <= furthest;
            const done = index < stepIndex;
            return (
              <Tabs.Trigger
                key={step.id}
                value={step.id}
                disabled={!reached}
                className={cx(
                  'flex items-center gap-2 rounded-sm px-2.5 py-1.5 text-left text-sm',
                  'data-[state=active]:bg-accent-soft data-[state=active]:text-accent-text data-[state=active]:font-medium',
                  reached ? 'text-ink-2 hover:bg-hover hover:text-ink' : 'text-muted',
                )}
              >
                <span
                  aria-hidden
                  className={cx(
                    'flex size-4 shrink-0 items-center justify-center rounded-pill text-[10px]',
                    done ? 'bg-ok text-on-accent' : 'bg-sunken',
                  )}
                >
                  {done ? <CheckIcon className="size-3" /> : index + 1}
                </span>
                {step.title}
              </Tabs.Trigger>
            );
          })}
        </Tabs.List>
        <div className="min-w-0 flex-1">
          <h1 className="text-lg font-semibold text-ink">{currentStep.heading}</h1>
          <Tabs.Content value={currentStep.id} tabIndex={-1} className="mt-4 outline-none">
            <Step />
          </Tabs.Content>
        </div>
      </Tabs.Root>
    </div>
  );
}
