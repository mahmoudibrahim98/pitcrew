// The running wizard's state and navigation, shared by the stepper and every step component.
// `WizardProvider` owns the state (`wizard-state.ts`); nothing here is persisted, so a reload
// starts the wizard over.

import { createContext, use, useCallback, useMemo, useState, type ReactNode } from 'react';
import { useOnboardingApi } from './api-context.tsx';
import { showDraftStep, useDraftStepAvailable } from './draft-board.tsx';
import { stepsFor, type StepMeta } from './steps.ts';
import { initialWizardState, type WizardState } from './wizard-state.ts';

export type WizardPatch = Partial<WizardState> | ((state: WizardState) => Partial<WizardState>);

export interface WizardContextValue {
  /** The steps the API can serve (`stepsFor`), the draft step only when it applies. */
  steps: readonly StepMeta[];
  state: WizardState;
  patch(update: WizardPatch): void;
  stepIndex: number;
  /** The furthest step index reached; the stepper only lets you jump to a step at or before this. */
  furthest: number;
  currentStep: StepMeta;
  goTo(index: number): void;
  /** Moves to the next step (or does nothing on the last one; the Done step navigates itself). */
  next(): void;
  back(): void;
  /** Like `next`, for a step the user chose to skip; both advance the same way. */
  skip(): void;
  /** The machine name field's label: this computer's, or a remote machine's. */
  machineLabel: string;
}

/** What the first run starts from, when the workspace is not this computer's own hub. */
export interface WizardDefaults {
  machineName?: string | undefined;
  machineLabel?: string | undefined;
}

export const LOCAL_MACHINE_LABEL = 'This machine’s name';

/** `steps` is always non-empty (`steps.ts`); this only ever throws on a genuinely bad index. */
function stepAt(steps: readonly StepMeta[], index: number): StepMeta {
  const step = steps[Math.min(Math.max(index, 0), steps.length - 1)];
  if (step === undefined) throw new Error('WizardProvider: no steps defined');
  return step;
}

const WizardContext = createContext<WizardContextValue | null>(null);

export function useWizard(): WizardContextValue {
  const ctx = use(WizardContext);
  if (ctx === null) throw new Error('useWizard must be used inside <WizardProvider>');
  return ctx;
}

/** Inside an `OnboardingApiProvider`: the steps follow what its API can do. */
export function WizardProvider({ defaults = {}, children }: { defaults?: WizardDefaults; children: ReactNode }) {
  const api = useOnboardingApi();
  const all = useMemo(() => stepsFor(api), [api]);
  const [state, setState] = useState<WizardState>(() => initialWizardState(defaults.machineName));
  // The optional draft step shows only where a hub drafts boards and there is something to draft.
  const drafts = useDraftStepAvailable();
  const draft = drafts && showDraftStep(state);
  const steps = useMemo(() => all.filter((step) => step.id !== 'draft' || draft), [all, draft]);
  const [stepIndex, setStepIndex] = useState(0);
  const [furthest, setFurthest] = useState(0);

  // Stable identities: several step components call `patch` from inside a `useEffect` that starts
  // a stream or a fetch (scan, install, machine check). If `patch` were a new function every
  // render, that effect's cleanup would run (and cancel the stream) on the very next render its
  // own `patch` call causes — `useCallback` with no captured state keeps it the same function for
  // the wizard's whole lifetime.
  const patch = useCallback((update: WizardPatch) => {
    setState((s) => ({ ...s, ...(typeof update === 'function' ? update(s) : update) }));
  }, []);

  const goTo = useCallback(
    (index: number) => {
      if (index < 0 || index > furthest || index >= steps.length) return;
      setStepIndex(index);
    },
    [furthest, steps.length],
  );

  const next = useCallback(() => {
    setStepIndex((i) => {
      const target = Math.min(i + 1, steps.length - 1);
      setFurthest((f) => Math.max(f, target));
      return target;
    });
  }, [steps.length]);

  const back = useCallback(() => setStepIndex((i) => Math.max(0, i - 1)), []);

  const value: WizardContextValue = {
    steps,
    state,
    patch,
    stepIndex,
    furthest,
    currentStep: stepAt(steps, stepIndex),
    goTo,
    next,
    back,
    skip: next,
    machineLabel: defaults.machineLabel ?? LOCAL_MACHINE_LABEL,
  };

  return <WizardContext value={value}>{children}</WizardContext>;
}
