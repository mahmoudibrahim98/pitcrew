// Step 5: one row per engine and account. "Sign in" runs the CLI's own login in a terminal on the
// target machine (ADR-0010: PitCrew never reads or copies its OAuth tokens) and links to where
// that terminal would open; the Agent console (stream M) is not registered in this worktree yet,
// so the link resolves to the shell's placeholder session page for now.

import { useEffect, useRef, useState } from 'react';
import { StatusPill } from '../../design/index.ts';
import { paths, useWorkspaceId } from '../../shell/index.ts';
import type { Engine } from '../../data/index.ts';
import { useOnboardingApi } from '../api-context.tsx';
import { StepFooter } from '../step-footer.tsx';
import { useWizard } from '../wizard-context.tsx';

const ENGINE_LABEL: Record<Engine, string> = {
  claude: 'Claude Code',
  codex: 'Codex',
  opencode: 'OpenCode',
};

export function SignInStep() {
  const { state, patch, next, skip } = useWizard();
  const api = useOnboardingApi();
  const ws = useWorkspaceId();
  const [starting, setStarting] = useState<Engine | null>(null);
  const [sessions, setSessions] = useState<Partial<Record<Engine, string>>>({});
  const loaded = useRef(false);

  useEffect(() => {
    if (loaded.current) return;
    loaded.current = true;
    void api.agentAccounts().then((accounts) => patch({ accounts }));
  }, [api, patch]);

  async function signIn(engine: Engine) {
    setStarting(engine);
    try {
      const { terminalSessionId } = await api.startSignIn(engine, state.primaryMachine);
      setSessions((s) => ({ ...s, [engine]: terminalSessionId }));
      const accounts = await api.agentAccounts();
      patch({ accounts });
    } finally {
      setStarting(null);
    }
  }

  return (
    <form
      onSubmit={(e) => {
        e.preventDefault();
        next();
      }}
    >
      <ul className="divide-y divide-line rounded-sm border border-line">
        {state.accounts.map((account) => {
          const terminalSessionId = sessions[account.engine];
          return (
            <li key={account.engine} className="flex items-center justify-between gap-3 px-3 py-2.5">
              <div>
                <p className="text-sm text-ink">{ENGINE_LABEL[account.engine]}</p>
                {account.account !== undefined && <p className="text-xs text-ink-2">{account.account}</p>}
              </div>
              <div className="flex items-center gap-2">
                <StatusPill tone={account.signedIn ? 'ok' : 'neutral'}>
                  {account.signedIn ? 'Signed in' : 'Not signed in'}
                </StatusPill>
                {terminalSessionId !== undefined && (
                  <a
                    href={paths.session(ws, terminalSessionId)}
                    className="text-xs text-accent-text underline underline-offset-2"
                  >
                    Open terminal
                  </a>
                )}
                <button
                  type="button"
                  onClick={() => void signIn(account.engine)}
                  disabled={starting !== null}
                  className="h-6 rounded-sm border border-line-2 px-2 text-xs text-ink-2 hover:bg-hover disabled:opacity-50"
                >
                  {starting === account.engine ? 'Opening…' : account.signedIn ? 'Sign in again' : 'Sign in'}
                </button>
              </div>
            </li>
          );
        })}
      </ul>
      <StepFooter nextLabel="Continue" onSkip={skip} skipLabel="Skip for now" />
    </form>
  );
}
