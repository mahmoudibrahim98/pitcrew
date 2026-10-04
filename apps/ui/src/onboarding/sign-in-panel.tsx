// Signing in to the agent CLIs on a machine: one row per CLI with its account as the CLI's own
// status command reports it, and "Sign in", which runs the CLI's own login (`claude auth login`,
// `codex login`, `opencode auth login`) in a terminal on the machine, shown below in the console's
// terminal view, which the person drives. PitCrew never reads or copies what the login stores
// (ADR-0010). While the login runs the panel asks now and then whether it has ended; then it asks
// the CLIs again. Used by the first run's Sign in step and by the connect wizard.

import { useEffect, useEffectEvent, useState } from 'react';
import { StatusPill } from '../design/index.ts';
import type { Engine } from '../data/index.ts';
import type { AgentAccount, MachineTarget, OnboardingApi, SignInMethod } from './api.ts';
import { machineTargetLabel } from './api.ts';
import { SignInTerminal } from './sign-in-terminal.tsx';

const ENGINE_LABEL: Record<Engine, string> = {
  claude: 'Claude Code',
  codex: 'Codex',
  opencode: 'OpenCode',
};

/** How often the panel asks whether a login has ended. */
export const POLL_MS = 2000;

function messageOf(error: unknown): string {
  return error instanceof Error ? error.message : String(error);
}

function pill(account: AgentAccount): { tone: 'ok' | 'neutral' | 'warn'; word: string } {
  if (!account.installed) return { tone: 'neutral', word: 'Not installed' };
  if (account.signedIn === true) return { tone: 'ok', word: 'Signed in' };
  if (account.signedIn === false) return { tone: 'neutral', word: 'Not signed in' };
  return { tone: 'warn', word: 'Could not tell' };
}

interface OpenSignIn {
  engine: Engine;
  terminal: string;
  command: string[];
  ended: boolean;
}

export function SignInPanel({
  api,
  target,
  machineLabel,
  cached,
  onAccounts,
}: {
  api: Pick<OnboardingApi, 'agentAccounts' | 'startSignIn' | 'signInRunning'>;
  target: MachineTarget;
  /** The machine's name for people, when `target`'s own label would not say it (a remote hub's). */
  machineLabel?: string | undefined;
  /** Accounts already read, shown at once instead of asking the CLIs again. */
  cached?: AgentAccount[] | undefined;
  /** Each time the accounts have been read. */
  onAccounts?: ((accounts: AgentAccount[]) => void) | undefined;
}) {
  const [accounts, setAccounts] = useState<AgentAccount[]>(cached ?? []);
  const [reading, setReading] = useState(cached === undefined);
  const [attempt, setAttempt] = useState(0);
  const [starting, setStarting] = useState<Engine | null>(null);
  const [open, setOpen] = useState<OpenSignIn | undefined>();
  const [failed, setFailed] = useState<string | undefined>();
  // The caller's, which may be a new function each render: not a reason to ask the CLIs again.
  const reportAccounts = useEffectEvent((read: AgentAccount[]) => onAccounts?.(read));

  useEffect(() => {
    if (!reading) return;
    let live = true;
    api.agentAccounts().then(
      (read) => {
        if (!live) return;
        setAccounts(read);
        setReading(false);
        reportAccounts(read);
      },
      (error: unknown) => {
        if (!live) return;
        setReading(false);
        setFailed(messageOf(error));
      },
    );
    return () => {
      live = false;
    };
  }, [api, reading, attempt]);

  // While a login runs, ask now and then whether it has ended; then ask the CLIs again.
  useEffect(() => {
    if (open === undefined || open.ended) return;
    let live = true;
    const { engine, terminal } = open;
    const timer = setInterval(() => {
      api.signInRunning(engine, target).then(
        (running) => {
          if (!live || running) return;
          setOpen((o) => (o?.terminal === terminal ? { ...o, ended: true } : o));
          setReading(true);
          setAttempt((n) => n + 1);
        },
        () => undefined,
      );
    }, POLL_MS);
    return () => {
      live = false;
      clearInterval(timer);
    };
  }, [api, open, target]);

  async function signIn(engine: Engine, method?: SignInMethod) {
    setStarting(engine);
    setFailed(undefined);
    try {
      const started = await api.startSignIn(engine, target, method);
      setOpen({ engine, terminal: started.terminalSessionId, command: started.command, ended: false });
    } catch (error) {
      setFailed(messageOf(error));
    } finally {
      setStarting(null);
    }
  }

  function readAgain() {
    setFailed(undefined);
    setReading(true);
    setAttempt((n) => n + 1);
  }

  return (
    <div>
      <p className="text-sm text-ink-2">
        Each CLI signs in with its own login, in a terminal on {machineLabel ?? machineTargetLabel(target)}. PitCrew passes your keys
        to it and keeps nothing of the login.
      </p>
      <ul className="mt-3 divide-y divide-line rounded-sm border border-line">
        {reading && accounts.length === 0 && (
          <li className="px-3 py-2.5 text-sm text-ink-2" aria-live="polite">
            Asking each CLI…
          </li>
        )}
        {accounts.map((account) => {
          const { tone, word } = pill(account);
          const label = ENGINE_LABEL[account.engine];
          const note = account.account ?? account.detail;
          return (
            <li key={account.engine} className="flex items-center justify-between gap-3 px-3 py-2.5">
              <div className="min-w-0">
                <p className="text-sm text-ink">{label}</p>
                {note !== undefined && <p className="text-xs break-words text-ink-2">{note}</p>}
              </div>
              <div className="flex shrink-0 items-center gap-2">
                <StatusPill tone={tone}>{word}</StatusPill>
                {account.installed && account.engine === 'codex' && (
                  <button
                    type="button"
                    onClick={() => void signIn(account.engine, 'device-code')}
                    disabled={starting !== null}
                    aria-label={`Sign in to ${label} with a code`}
                    className="h-6 rounded-sm border border-line-2 px-2 text-xs text-ink-2 hover:bg-hover disabled:opacity-50"
                  >
                    With a code
                  </button>
                )}
                {account.installed && (
                  <button
                    type="button"
                    onClick={() => void signIn(account.engine)}
                    disabled={starting !== null}
                    aria-label={`${account.signedIn === true ? 'Sign in again to' : 'Sign in to'} ${label}`}
                    className="h-6 rounded-sm border border-line-2 px-2 text-xs text-ink-2 hover:bg-hover disabled:opacity-50"
                  >
                    {starting === account.engine ? 'Opening…' : account.signedIn === true ? 'Sign in again' : 'Sign in'}
                  </button>
                )}
              </div>
            </li>
          );
        })}
      </ul>
      {failed !== undefined && (
        <p role="alert" className="mt-3 text-sm text-risk">
          {failed}
        </p>
      )}
      {!reading && (
        <button
          type="button"
          onClick={readAgain}
          className="mt-3 h-7 rounded-sm border border-line-2 px-2.5 text-sm text-ink-2 hover:bg-hover"
        >
          Ask the CLIs again
        </button>
      )}

      {open !== undefined && (
        <section aria-label={`${ENGINE_LABEL[open.engine]} sign-in`} className="mt-4 flex flex-col gap-2">
          <p className="text-sm text-ink">
            <span className="font-mono">{open.command.join(' ')}</span>
            {open.ended ? ': the login has ended.' : ' runs below. Take control of the terminal to answer it.'}
          </p>
          <SignInTerminal key={open.terminal} terminal={open.terminal} />
        </section>
      )}
    </div>
  );
}
