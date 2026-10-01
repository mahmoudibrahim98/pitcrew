// Step 11: the CLI's own permission mode by default (ADR-0010: skipping permissions is an
// explicit opt-in), and the back office's on/off switch with its auto-accept cap.

import { useId, useState } from 'react';
import type { PermissionMode } from '../api.ts';
import { useOnboardingApi } from '../api-context.tsx';
import { StepFooter } from '../step-footer.tsx';
import { useWizard } from '../wizard-context.tsx';

const PERMISSION_LABEL: Record<PermissionMode, { label: string; hint: string }> = {
  default: { label: 'Default', hint: "The CLI's own prompts for risky actions." },
  plan: { label: 'Plan first', hint: 'Agents propose a plan before touching anything.' },
  'accept-edits': { label: 'Accept edits', hint: 'File edits run without asking; commands still prompt.' },
  'bypass-permissions': { label: 'Skip permissions', hint: 'Nothing prompts. An explicit, risky opt-in.' },
};

export function SafetyStep() {
  const { state, patch, next } = useWizard();
  const api = useOnboardingApi();
  const [busy, setBusy] = useState(false);
  const capsId = useId();

  async function submit() {
    setBusy(true);
    try {
      await api.saveSafety(state.safety);
      next();
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
      <fieldset className="flex flex-col gap-1.5">
        <legend className="text-sm font-medium text-ink">Permission mode</legend>
        {(Object.keys(PERMISSION_LABEL) as PermissionMode[]).map((mode) => (
          <label key={mode} className="flex items-start gap-2 text-sm text-ink">
            <input
              type="radio"
              name="permission-mode"
              className="mt-0.5"
              checked={state.safety.permissionMode === mode}
              onChange={() => patch({ safety: { ...state.safety, permissionMode: mode } })}
            />
            <span>
              <span className="block">{PERMISSION_LABEL[mode].label}</span>
              <span className="block text-xs text-ink-2">{PERMISSION_LABEL[mode].hint}</span>
            </span>
          </label>
        ))}
      </fieldset>

      <fieldset className="mt-4 flex flex-col gap-2 rounded-sm border border-line p-3">
        <label className="flex items-center gap-2 text-sm font-medium text-ink">
          <input
            type="checkbox"
            checked={state.safety.backOfficeEnabled}
            onChange={(e) => patch({ safety: { ...state.safety, backOfficeEnabled: e.target.checked } })}
          />
          Let the back office accept low-risk actions automatically
        </label>
        {state.safety.backOfficeEnabled && (
          <div className="flex items-center gap-2 pl-6">
            <label htmlFor={capsId} className="text-xs text-ink-2">
              Up to
            </label>
            <input
              id={capsId}
              type="number"
              min={0}
              max={100}
              value={state.safety.backOfficeCaps.maxAutoAcceptPerHour}
              onChange={(e) =>
                patch({
                  safety: {
                    ...state.safety,
                    backOfficeCaps: { maxAutoAcceptPerHour: Number(e.target.value) },
                  },
                })
              }
              className="h-7 w-16 rounded-sm border border-line-2 bg-card px-2 text-sm text-ink"
            />
            <span className="text-xs text-ink-2">per hour, without asking.</span>
          </div>
        )}
      </fieldset>

      <StepFooter nextLabel="Continue" busy={busy} />
    </form>
  );
}
