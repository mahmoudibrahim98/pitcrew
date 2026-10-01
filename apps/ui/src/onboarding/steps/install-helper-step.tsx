// Step 4: pick a launcher (ADR-0009: direct, tmux, systemd-user or slurm), see the exact SLURM
// script before anything is submitted, then watch the deploy as a live log.

import { RadioGroup } from 'radix-ui';
import { useEffect, useRef } from 'react';
import { cx } from '../../lib/cx.ts';
import type { Launcher, Streamed } from '../api.ts';
import { useOnboardingApi } from '../api-context.tsx';
import { StepFooter } from '../step-footer.tsx';
import { useWizard } from '../wizard-context.tsx';

const LAUNCHER_LABEL: Record<Launcher, string> = {
  direct: 'Direct (setsid)',
  tmux: 'tmux',
  'systemd-user': 'systemd user service',
  slurm: 'SLURM batch job',
};

export function InstallHelperStep() {
  const { state, patch, next } = useWizard();
  const api = useOnboardingApi();
  const streamed = useRef<Streamed | null>(null);
  const optionsFor = useRef<string | null>(null);

  useEffect(() => {
    const key = JSON.stringify(state.primaryMachine);
    if (optionsFor.current === key) return;
    optionsFor.current = key;
    void api.launcherOptions(state.primaryMachine).then((options) => {
      const recommended = options.find((o) => o.recommended);
      patch({
        launcherOptions: options,
        ...(recommended === undefined ? {} : { launcher: recommended.launcher }),
      });
    });
  }, [api, state.primaryMachine, patch]);

  useEffect(() => () => streamed.current?.cancel(), []);

  function install() {
    patch({ installLog: [], installStatus: 'running', installError: undefined, slurmScript: undefined });
    streamed.current = api.streamInstallHelper({ machine: state.primaryMachine, launcher: state.launcher }, (event) => {
      if (event.type === 'log') {
        patch((s) => ({ installLog: [...s.installLog, event.line] }));
      } else if (event.type === 'script-preview') {
        patch({ slurmScript: event.script });
      } else if (event.type === 'done') {
        patch({ installStatus: 'done' });
      } else {
        patch({ installStatus: 'error', installError: event.message });
      }
    });
  }

  const running = state.installStatus === 'running';
  const done = state.installStatus === 'done';

  return (
    <form
      onSubmit={(e) => {
        e.preventDefault();
        if (done) next();
        else install();
      }}
    >
      <fieldset className="flex flex-col gap-1.5" disabled={running || done}>
        <legend className="text-sm font-medium text-ink">Launcher</legend>
        <RadioGroup.Root
          value={state.launcher}
          onValueChange={(value) => patch({ launcher: value as Launcher })}
          className="flex flex-col gap-1.5"
        >
          {state.launcherOptions.map((option) => (
            <label
              key={option.launcher}
              className={cx(
                'flex items-center gap-2 rounded-sm border px-3 py-2 text-sm',
                option.unavailable !== undefined && 'opacity-50',
                state.launcher === option.launcher && option.unavailable === undefined
                  ? 'border-accent bg-accent-soft text-accent-text'
                  : 'border-line',
              )}
            >
              <RadioGroup.Item
                value={option.launcher}
                disabled={option.unavailable !== undefined}
                className="size-3.5 shrink-0 rounded-pill border border-line-2"
              >
                <RadioGroup.Indicator className="block size-full scale-50 rounded-pill bg-accent" />
              </RadioGroup.Item>
              <span className="flex-1">
                {LAUNCHER_LABEL[option.launcher]}
                {option.recommended && <span className="ml-1.5 text-xs text-ink-2">(recommended)</span>}
              </span>
              {option.unavailable !== undefined && <span className="text-xs text-ink-2">{option.unavailable}</span>}
            </label>
          ))}
        </RadioGroup.Root>
      </fieldset>

      {state.slurmScript !== undefined && (
        <div className="mt-4">
          <p className="text-sm font-medium text-ink">The exact script PitCrew will submit</p>
          <pre className="mt-1.5 max-h-48 overflow-auto rounded-sm border border-line bg-sunken p-3 text-xs text-ink-2">
            {state.slurmScript}
          </pre>
        </div>
      )}

      {(running || done || state.installLog.length > 0) && (
        <div
          role="log"
          aria-live="polite"
          aria-label="Install progress"
          className="mt-4 max-h-40 overflow-auto rounded-sm border border-line bg-sunken p-3 font-mono text-xs text-ink-2"
        >
          {state.installLog.map((line, i) => (
            <p key={i}>{line}</p>
          ))}
          {done && <p className="text-ok">Helper installed.</p>}
        </div>
      )}

      {state.installStatus === 'error' && (
        <p role="alert" className="mt-3 text-sm text-risk">
          {state.installError}
        </p>
      )}

      {!running && !done && (
        <div className="mt-4">
          <button
            type="submit"
            className="h-7 rounded-sm bg-accent px-2.5 text-sm font-medium text-on-accent hover:bg-accent-hover"
          >
            Install the helper
          </button>
        </div>
      )}

      <StepFooter nextLabel="Continue" nextDisabled={!done} busy={running} />
    </form>
  );
}
