// Step 3: CLI versions, tmux, git and gh, disk, SLURM — each with its own "Fix" button. Re-runs
// automatically when the target machine changes (e.g. the user went back and picked another one).

import { useEffect, useRef, useState } from 'react';
import { StatusPill } from '../../design/index.ts';
import type { CheckRowId, CheckRowStatus } from '../api.ts';
import { machineTargetLabel } from '../api.ts';
import { useOnboardingApi } from '../api-context.tsx';
import { StepFooter } from '../step-footer.tsx';
import { useWizard } from '../wizard-context.tsx';

const TONE: Record<CheckRowStatus, 'ok' | 'warn' | 'risk' | 'progress'> = {
  ok: 'ok',
  warn: 'warn',
  missing: 'risk',
  checking: 'progress',
};

const STATUS_WORD: Record<CheckRowStatus, string> = {
  ok: 'OK',
  warn: 'Needs attention',
  missing: 'Missing',
  checking: 'Checking…',
};

export function MachineCheckStep() {
  const { state, patch, next } = useWizard();
  const api = useOnboardingApi();
  const [fixing, setFixing] = useState<CheckRowId | null>(null);
  const target = state.primaryMachine;
  const ranFor = useRef<string | null>(null);

  useEffect(() => {
    const key = JSON.stringify(target);
    if (ranFor.current === key) return;
    ranFor.current = key;
    patch({ machineCheckStatus: 'running' });
    void api.checkMachine(target).then((result) => {
      patch({ machineCheck: result, machineCheckStatus: 'done' });
    });
  }, [api, target, patch]);

  async function fix(row: CheckRowId) {
    setFixing(row);
    try {
      const fixed = await api.fixMachineRow(target, row);
      patch((s) =>
        s.machineCheck === undefined
          ? {}
          : { machineCheck: { ...s.machineCheck, rows: s.machineCheck.rows.map((r) => (r.id === row ? fixed : r)) } },
      );
    } finally {
      setFixing(null);
    }
  }

  const rows = state.machineCheck?.rows ?? [];
  const loading = state.machineCheckStatus !== 'done';

  return (
    <form
      onSubmit={(e) => {
        e.preventDefault();
        next();
      }}
    >
      <p className="text-sm text-ink-2">On {machineTargetLabel(target)}:</p>
      <ul className="mt-3 divide-y divide-line rounded-sm border border-line">
        {loading && (
          <li className="px-3 py-2.5 text-sm text-ink-2" aria-live="polite">
            Checking the machine…
          </li>
        )}
        {rows.map((row) => (
          <li key={row.id} className="flex items-center justify-between gap-3 px-3 py-2.5">
            <div className="min-w-0">
              <p className="text-sm text-ink">{row.label}</p>
              {row.detail !== undefined && <p className="truncate text-xs text-ink-2">{row.detail}</p>}
            </div>
            <div className="flex shrink-0 items-center gap-2">
              <StatusPill tone={TONE[row.status]}>{STATUS_WORD[row.status]}</StatusPill>
              {row.fixable && row.status !== 'ok' && (
                <button
                  type="button"
                  onClick={() => void fix(row.id)}
                  disabled={fixing !== null}
                  className="h-6 rounded-sm border border-line-2 px-2 text-xs text-ink-2 hover:bg-hover disabled:opacity-50"
                >
                  {fixing === row.id ? 'Fixing…' : 'Fix'}
                </button>
              )}
            </div>
          </li>
        ))}
      </ul>
      <StepFooter nextLabel="Continue" nextDisabled={loading} />
    </form>
  );
}
