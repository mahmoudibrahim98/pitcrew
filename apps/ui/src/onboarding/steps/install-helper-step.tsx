// Step 4: pick a launcher (ADR-0009: direct, tmux, systemd-user or slurm), see the exact SLURM
// script before anything is submitted, then watch the deploy as a live log.

import { RadioGroup } from 'radix-ui';
import { useEffect, useRef } from 'react';
import { cx } from '../../lib/cx.ts';
import type { Launcher, Streamed } from '../api.ts';
import { targetKey } from '../api.ts';
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
  const key = targetKey(state.primaryMachine);
  const options = state.launcherOptionsByTarget[key];
  // Falls back to 'direct' only until the options (and so a recommended default) have loaded; once
  // loaded, a target with no entry yet is seeded from `recommended` — see the effect below. A
  // launcher the person already chose for this target is never overwritten by that seeding (review
  // r1, item 1): revisiting via Back/Forward reads the same `launcherChoiceByTarget[key]` back.
  const launcher = state.launcherChoiceByTarget[key] ?? 'direct';

  useEffect(() => {
    if (options !== undefined) return;
    void api.launcherOptions(state.primaryMachine).then((fetched) => {
      const recommended = fetched.find((o) => o.recommended);
      patch((s) => ({
        launcherOptionsByTarget: { ...s.launcherOptionsByTarget, [key]: fetched },
        launcherChoiceByTarget:
          s.launcherChoiceByTarget[key] !== undefined || recommended === undefined
            ? s.launcherChoiceByTarget
            : { ...s.launcherChoiceByTarget, [key]: recommended.launcher },
      }));
    });
  }, [api, state.primaryMachine, key, options, patch]);

  useEffect(() => () => streamed.current?.cancel(), []);

  function install() {
    patch({ installLog: [], installStatus: 'running', installError: undefined, slurmScript: undefined });
    streamed.current = api.streamInstallHelper({ machine: state.primaryMachine, launcher }, (event) => {
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
          value={launcher}
          onValueChange={(value) =>
            patch((s) => ({ launcherChoiceByTarget: { ...s.launcherChoiceByTarget, [key]: value as Launcher } }))
          }
          className="flex flex-col gap-1.5"
        >
          {(options ?? []).map((option) => (
            <label
              key={option.launcher}
              className={cx(
                'flex items-center gap-2 rounded-sm border px-3 py-2 text-sm',
                option.unavailable !== undefined && 'opacity-50',
                launcher === option.launcher && option.unavailable === undefined
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
