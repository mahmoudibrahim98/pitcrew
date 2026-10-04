// Step 3: CLI versions, tmux, git and gh, disk, SLURM: each with a short reason, and, where PitCrew
// can help, a fix. A fix never installs anything: "Install…" says where the tool is installed from
// (`install-pages.ts`, and opens that page in a browser tab outside the desktop app), and "Check
// again" asks the machine afresh once the person has installed it. Re-runs automatically when the
// target machine changes (e.g. the user went back and picked another one). A machine that is
// checked later, as it is connected (`deferred`: an SSH host, a WSL distro, an HPC login node),
// says so instead of rows: that is not an error.

import { useEffect, useState } from 'react';
import type { CheckRowId, MachineCheckRow } from '../api.ts';
import { machineTargetLabel, targetKey } from '../api.ts';
import { useOnboardingApi } from '../api-context.tsx';
import { CheckRowLine, InstallPageNote } from '../check-rows.tsx';
import { StepFooter } from '../step-footer.tsx';
import { useWizard } from '../wizard-context.tsx';

function messageOf(error: unknown): string {
  return error instanceof Error ? error.message : String(error);
}

export function MachineCheckStep() {
  const { state, patch, next } = useWizard();
  const api = useOnboardingApi();
  const [fixing, setFixing] = useState<CheckRowId | null>(null);
  /** Rows whose "Install…" was pressed: their install page is shown. */
  const [opened, setOpened] = useState<ReadonlySet<CheckRowId>>(new Set());
  const [failed, setFailed] = useState<string | undefined>();
  /** Counts "Check again" presses, so one after a failed check asks again. */
  const [attempt, setAttempt] = useState(0);
  const target = state.primaryMachine;
  const key = targetKey(target);
  const cached = state.machineCheckByTarget[key];

  // Cached per target: Back to a target already checked (including any row already fixed, since
  // `fixMachineRow` writes its result back into the same cache entry) shows it again instead of
  // re-running the check and briefly flashing "Checking the machine…". "Check again" clears it.
  useEffect(() => {
    if (cached !== undefined) return;
    let live = true;
    api.checkMachine(target).then(
      (result) => {
        if (live) patch((s) => ({ machineCheckByTarget: { ...s.machineCheckByTarget, [key]: result } }));
      },
      (error: unknown) => {
        if (live) setFailed(messageOf(error));
      },
    );
    return () => {
      live = false;
    };
  }, [api, target, key, cached, patch, attempt]);

  function checkAgain() {
    setOpened(new Set());
    setFailed(undefined);
    patch((s) => ({
      machineCheckByTarget: Object.fromEntries(Object.entries(s.machineCheckByTarget).filter(([k]) => k !== key)),
    }));
    setAttempt((n) => n + 1);
  }

  async function fix(row: MachineCheckRow) {
    setFixing(row.id);
    setFailed(undefined);
    try {
      const fixed = await api.fixMachineRow(target, row.id);
      if (row.fix === 'install-page' && fixed.status !== 'ok') setOpened((o) => new Set(o).add(row.id));
      patch((s) => {
        const current = s.machineCheckByTarget[key];
        if (current === undefined) return {};
        return {
          machineCheckByTarget: {
            ...s.machineCheckByTarget,
            [key]: { ...current, rows: current.rows.map((r) => (r.id === row.id ? fixed : r)) },
          },
        };
      });
    } catch (error) {
      setFailed(messageOf(error));
    } finally {
      setFixing(null);
    }
  }

  const rows = cached?.rows ?? [];
  const loading = cached === undefined && failed === undefined;
  const deferred = cached?.deferred;

  return (
    <form
      onSubmit={(e) => {
        e.preventDefault();
        next();
      }}
    >
      <p className="text-sm text-ink-2">On {state.setup.machineName.trim() || machineTargetLabel(target)}:</p>
      {deferred !== undefined && (
        <p className="mt-3 rounded-sm border border-line px-3 py-2.5 text-sm text-ink-2">{deferred}</p>
      )}
      <ul className={deferred === undefined ? 'mt-3 divide-y divide-line rounded-sm border border-line' : 'hidden'}>
        {loading && (
          <li className="px-3 py-2.5 text-sm text-ink-2" aria-live="polite">
            Checking the machine…
          </li>
        )}
        {rows.map((row) => {
          const install = row.fix === 'install-page';
          const fixable = row.fixable && row.status !== 'ok' && row.fix !== 'install-helper';
          return (
            <CheckRowLine
              key={row.id}
              row={row}
              note={opened.has(row.id) && row.status !== 'ok' ? <InstallPageNote row={row} /> : undefined}
              action={
                fixable ? (
                  <button
                    type="button"
                    onClick={() => void fix(row)}
                    disabled={fixing !== null}
                    aria-label={install ? `Install ${row.label}…` : `Fix ${row.label}`}
                    className="h-6 rounded-sm border border-line-2 px-2 text-xs text-ink-2 hover:bg-hover disabled:opacity-50"
                  >
                    {fixing === row.id ? 'Checking…' : install ? 'Install…' : 'Fix'}
                  </button>
                ) : undefined
              }
            />
          );
        })}
      </ul>
      {failed !== undefined && (
        <p role="alert" className="mt-3 text-sm text-risk">
          {failed}
        </p>
      )}
      {!loading && deferred === undefined && (
        <div className="mt-3">
          <button
            type="button"
            onClick={checkAgain}
            disabled={fixing !== null}
            className="h-7 rounded-sm border border-line-2 px-2.5 text-sm text-ink-2 hover:bg-hover disabled:opacity-50"
          >
            Check again
          </button>
          <p className="mt-1.5 text-xs text-ink-2">
            PitCrew installs nothing on the machine: install what is missing yourself, then check again.
          </p>
        </div>
      )}
      <StepFooter nextLabel="Continue" nextDisabled={cached === undefined} />
    </form>
  );
}
