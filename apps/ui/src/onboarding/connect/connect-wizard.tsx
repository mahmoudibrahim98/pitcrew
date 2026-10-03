// Connect a remote machine (a server, a login node, or a SLURM node through its login node), over
// the desktop gateway's remote commands (desktop-gateway.md, "Remote workspaces"):
//   Host → Probe → Launcher → Review → Connect → Setup → Done.
// Nothing changes on the remote until the person presses Connect on the Review step, which shows
// the plan's steps and, for SLURM, the exact job script. A plan the gateway refuses (expired, or
// already used) sends the person back to Review with a fresh plan: a plan is never submitted
// without being shown. SSH's questions arrive meanwhile through the shell's prompt dialog.
//
// Every remote call starts from a button press, never from an effect, so React's development
// double-mount cannot run one twice (two password prompts, or one plan submitted twice).

import { useEffect, useId, useMemo, useRef, useState, type ReactNode, type RefObject } from 'react';
import {
  GatewayError,
  useGatewayWorkspace,
  useSetUp,
  useWorkspace,
  WorkspaceScope,
  type GatewayWorkspace,
  type JobOptions,
  type RemoteGateway,
  type RemoteLauncher,
  type RemotePlan,
  type RemotePlanRequest,
  type RemoteProbe,
  type RemoteProgress,
} from '../../data/index.ts';
import { Button, CheckIcon, Dialog, DialogContent, DialogFooter } from '../../design/index.ts';
import { cx } from '../../lib/cx.ts';
import type { WslDistro, WslTarget } from '../../data/remote.ts';
import type { DiscoveredHost } from '../api.ts';
import { createHubOnboardingApi } from '../hub-api.ts';
import { SetupForm } from '../setup-form.tsx';
import { checkCpus, checkHost, checkJobText, scriptHazards, type SetupValues } from '../validation.ts';

type Step = 'host' | 'probe' | 'launcher' | 'review' | 'connect' | 'setup' | 'done';

const STEPS: { id: Step; title: string }[] = [
  { id: 'host', title: 'Host' },
  { id: 'probe', title: 'Probe' },
  { id: 'launcher', title: 'Launcher' },
  { id: 'review', title: 'Review' },
  { id: 'connect', title: 'Connect' },
  { id: 'setup', title: 'Setup' },
  { id: 'done', title: 'Done' },
];

/** The SLURM fields, as typed. */
interface JobDraft {
  site: string;
  partition: string;
  account: string;
  qos: string;
  time: string;
  cpus: string;
  memory: string;
  gpus: string;
}

const JOB_FIELDS: { key: keyof JobDraft; label: string; hint?: string }[] = [
  { key: 'site', label: 'Site recipe', hint: 'Your site’s recipe, if it has one; generic otherwise.' },
  { key: 'partition', label: 'Partition' },
  { key: 'account', label: 'Account' },
  { key: 'qos', label: 'QoS' },
  { key: 'time', label: 'Time limit', hint: 'For example 08:00:00, or 2-00:00:00.' },
  { key: 'cpus', label: 'CPUs' },
  { key: 'memory', label: 'Memory', hint: 'For example 16G.' },
  { key: 'gpus', label: 'GPUs', hint: 'For example 2, or a100:2.' },
];

/**
 * Whether the probe found a tmux the tmux launcher can use (3.2 or newer): `false` for an older
 * one, `undefined` when it is not known (the gateway did not say).
 */
export function tmuxUsable(probe: RemoteProbe | undefined): boolean | undefined {
  const version = probe?.tmux?.version;
  const match = version === undefined ? null : /(\d+)\.(\d+)/.exec(version);
  if (match === null) return undefined;
  const major = Number(match[1]);
  const minor = Number(match[2]);
  return major > 3 || (major === 3 && minor >= 2);
}

/** The launcher to start from, given what the probe found. */
function usableLauncher(current: RemoteLauncher, probe: RemoteProbe): RemoteLauncher {
  if (current === 'slurm' && probe.slurm === undefined) return tmuxUsable(probe) === false ? 'direct' : 'tmux';
  if (current === 'tmux' && tmuxUsable(probe) === false) return 'direct';
  return current;
}

const EMPTY_JOB: JobDraft = { site: '', partition: '', account: '', qos: '', time: '', cpus: '', memory: '', gpus: '' };

const LAUNCHER_ORDER: readonly RemoteLauncher[] = ['direct', 'tmux', 'slurm'];

const LAUNCHERS: Record<RemoteLauncher, { label: string; hint: string }> = {
  direct: { label: 'Directly', hint: 'Starts PitCrew in the background on the host.' },
  tmux: { label: 'In tmux', hint: 'Starts PitCrew in a tmux session, which outlives your SSH connection.' },
  slurm: { label: 'As a SLURM job', hint: 'Submits PitCrew as a batch job on a compute node, until the job’s time limit.' },
};

interface State {
  step: Step;
  /** The host picked from the list, or typed. */
  picked: string;
  typed: string;
  source: 'list' | 'typed' | 'wsl';
  target?: WslTarget | undefined;
  hostError?: string | undefined;
  /** The host the later steps are about. */
  host: string;
  probe?: RemoteProbe | undefined;
  probeError?: string | undefined;
  launcher: RemoteLauncher;
  job: JobDraft;
  jobErrors: Partial<Record<keyof JobDraft, string>>;
  plan?: RemotePlan | undefined;
  planError?: string | undefined;
  /** Why Review shows a fresh plan. */
  notice?: string | undefined;
  progress: RemoteProgress[];
  /** The add ended without a workspace: its error's message (the step and detail come from `progress`). */
  failure?: { message: string; cancelled: boolean } | undefined;
  /** The person asked to stop the add (`gateway_remote_cancel`), and why that did not work, if not. */
  cancelling?: boolean | undefined;
  cancelError?: string | undefined;
  workspace?: GatewayWorkspace | undefined;
  setup: SetupValues;
  handleEdited: boolean;
}

const INITIAL: State = {
  step: 'host',
  picked: '',
  typed: '',
  source: 'list',
  host: '',
  launcher: 'tmux',
  job: EMPTY_JOB,
  jobErrors: {},
  progress: [],
  setup: { workspaceName: '', personName: '', handle: '', machineName: '' },
  handleEdited: false,
};

function messageOf(error: unknown): string {
  return error instanceof Error && error.message !== '' ? error.message : 'Something went wrong.';
}

/** The plan request: the job's options only for SLURM, and only those given. */
export function planRequest(host: string, launcher: RemoteLauncher, job: JobDraft): RemotePlanRequest {
  if (launcher !== 'slurm') return { host, launcher };
  const options: JobOptions = {};
  const text = (value: string) => value.trim();
  if (text(job.partition) !== '') options.partition = text(job.partition);
  if (text(job.account) !== '') options.account = text(job.account);
  if (text(job.qos) !== '') options.qos = text(job.qos);
  if (text(job.time) !== '') options.time = text(job.time);
  if (text(job.cpus) !== '') options.cpus = Number(text(job.cpus));
  if (text(job.memory) !== '') options.memory = text(job.memory);
  if (text(job.gpus) !== '') options.gpus = text(job.gpus);
  return {
    host,
    launcher,
    ...(text(job.site) === '' ? {} : { site: text(job.site) }),
    ...(Object.keys(options).length === 0 ? {} : { job: options }),
  };
}

/** The step of the last progress message: the whole add's outcome, not one of the plan's steps. */
const ADD = 'add';

/** Progress by step, in the order steps first appeared, each with its latest state. */
function merge(progress: readonly RemoteProgress[], next: RemoteProgress): RemoteProgress[] {
  const index = progress.findIndex((p) => p.step === next.step);
  return index === -1 ? [...progress, next] : progress.map((p, i) => (i === index ? next : p));
}

export function ConnectWizard({
  remote,
  onOpen,
  onCancel,
  onLeave,
}: {
  remote: RemoteGateway;
  /** Opens the new workspace. */
  onOpen(workspace: GatewayWorkspace): void;
  /** Gives up before anything changed on the remote. */
  onCancel(): void;
  /**
   * Back to PitCrew while the add goes on in the gateway (its workspace shows up in the switcher
   * when it is ready), or with the new hub left to set up later.
   */
  onLeave(): void;
}) {
  const [state, setState] = useState<State>(INITIAL);
  const patch = (update: Partial<State>) => setState((s) => ({ ...s, ...update }));
  // Bumped by each remote call and each step change: a late answer for a step left behind is dropped.
  const generation = useRef(0);
  const begin = () => (generation.current += 1);
  const current = (g: number) => g === generation.current;
  const heading = useRef<HTMLHeadingElement>(null);

  // A new step's heading takes focus, so a screen reader hears where the person is.
  useEffect(() => heading.current?.focus(), [state.step]);

  function go(step: Step, update: Partial<State> = {}) {
    begin();
    patch({ ...update, step });
  }

  async function probe(host: string, target?: WslTarget) {
    const g = begin();
    patch({ step: 'probe', host, target, probe: undefined, probeError: undefined });
    try {
      const found = await remote.remoteProbe(target === undefined ? host : '', target);
      if (!current(g)) return;
      setState((s) => ({
        ...s,
        probe: found,
        // SLURM and tmux only where they can run; the site's default partition to start from.
        launcher: usableLauncher(s.launcher, found),
        job: s.job.partition === '' ? { ...s.job, partition: found.slurm?.defaultPartition ?? '' } : s.job,
      }));
    } catch (error) {
      if (current(g)) patch({ probeError: messageOf(error) });
    }
  }

  async function plan(request: RemotePlanRequest, notice?: string) {
    const g = begin();
    patch({ step: 'review', plan: undefined, planError: undefined, notice, failure: undefined });
    try {
      const fresh = await remote.remotePlan(request);
      if (current(g)) patch({ plan: fresh });
    } catch (error) {
      if (current(g)) patch({ planError: messageOf(error) });
    }
  }

  async function connect(shown: RemotePlan, request: RemotePlanRequest) {
    const g = begin();
    let started = false;
    patch({ step: 'connect', progress: [], failure: undefined, notice: undefined, cancelling: false, cancelError: undefined });
    try {
      const workspace = await remote.remoteAdd(shown.plan, (progress) => {
        started = true;
        if (current(g)) setState((s) => ({ ...s, progress: merge(s.progress, progress) }));
      });
      if (current(g)) {
        begin();
        setState((s) => ({
          ...s,
          step: 'setup',
          workspace,
          setup: { ...s.setup, workspaceName: s.setup.workspaceName || s.host, machineName: s.setup.machineName || s.host },
        }));
      }
    } catch (error) {
      if (!current(g)) return;
      if (error instanceof GatewayError && error.gateway === 'invalid' && !started) {
        // Refused before it started (expired, or already used): back to Review with a fresh plan,
        // shown before any Connect. A launch refused once under way is a failure like any other.
        void plan(request, `The gateway refused that plan (${error.message}). Here is a fresh one: check it, then press Connect.`);
        return;
      }
      setState((s) => ({ ...s, failure: { message: messageOf(error), cancelled: s.cancelling === true }, cancelling: false }));
    }
  }

  /** Stops the add on screen, after the person confirmed it; the add then ends `failed`. */
  async function cancelAdd(shown: RemotePlan) {
    patch({ cancelling: true, cancelError: undefined });
    try {
      await remote.remoteCancel(shown.plan);
    } catch (error) {
      const unknown =
        (error instanceof GatewayError && error.gateway === 'invalid') || /not found|unknown command/i.test(messageOf(error));
      patch({
        cancelling: false,
        cancelError: unknown
          ? 'This version of the desktop app cannot stop a connection yet. Leave it running, then remove the workspace once it shows up.'
          : `Could not stop it: ${messageOf(error)}`,
      });
    }
  }

  const request = state.target === undefined
    ? planRequest(state.host, state.launcher, state.job)
    : { host: '', target: state.target, launcher: state.launcher };
  const index = STEPS.findIndex((s) => s.id === state.step);

  return (
    <main className="min-h-dvh bg-bg text-ink">
      <div className="mx-auto flex max-w-4xl gap-10 px-8 py-10">
        <ol aria-label="Steps" className="flex w-48 shrink-0 flex-col gap-0.5">
          {STEPS.map((step, i) => (
            <li
              key={step.id}
              aria-current={i === index ? 'step' : undefined}
              className={cx(
                'flex items-center gap-2 rounded-sm px-2.5 py-1.5 text-sm',
                // Steps ahead are plain text, not controls: they keep a readable contrast.
                i === index ? 'bg-accent-soft font-medium text-accent-text' : 'text-ink-2',
              )}
            >
              <span
                aria-hidden
                className={cx(
                  'flex size-4 shrink-0 items-center justify-center rounded-pill text-[10px]',
                  i < index ? 'bg-ok text-on-accent' : 'bg-sunken',
                )}
              >
                {i < index ? <CheckIcon className="size-3" /> : i + 1}
              </span>
              {step.title}
              {i < index && <span className="sr-only"> (done)</span>}
            </li>
          ))}
        </ol>
        <div className="min-w-0 flex-1">
          {state.step === 'host' && (
            <HostStep
              remote={remote}
              state={state}
              patch={patch}
              heading={heading}
              onCancel={onCancel}
              onContinue={(host, target) => void probe(host, target)}
            />
          )}
          {state.step === 'probe' && (
            <ProbeStep
              state={state}
              heading={heading}
              onBack={() => go('host')}
              onRetry={() => void probe(state.host, state.target)}
              onContinue={() => go('launcher')}
              onCancel={onCancel}
            />
          )}
          {state.step === 'launcher' && (
            <LauncherStep
              state={state}
              patch={patch}
              heading={heading}
              onBack={() => go('probe')}
              onCancel={onCancel}
              onContinue={() => void plan(request)}
            />
          )}
          {state.step === 'review' && (
            <ReviewStep
              state={state}
              heading={heading}
              onBack={() => go('launcher', { notice: undefined })}
              onRetry={() => void plan(request, state.notice)}
              onCancel={onCancel}
              onConnect={(shown) => void connect(shown, request)}
            />
          )}
          {state.step === 'connect' && (
            <ConnectStep
              state={state}
              heading={heading}
              onReview={() => void plan(request, 'The last plan was used. Here is a fresh one: check it, then press Connect.')}
              onCancel={onCancel}
              onLeave={() => {
                // The add goes on in the gateway; nothing here waits for it any more.
                begin();
                onLeave();
              }}
              onStop={() => state.plan !== undefined && void cancelAdd(state.plan)}
            />
          )}
          {state.step === 'setup' && state.workspace !== undefined && (
            <SetupStep
              state={state}
              patch={patch}
              heading={heading}
              workspace={state.workspace}
              onDone={() => go('done')}
              onLater={onLeave}
            />
          )}
          {state.step === 'done' && state.workspace !== undefined && (
            <DoneStep host={state.host} workspace={state.workspace} heading={heading} onOpen={onOpen} />
          )}
        </div>
      </div>
    </main>
  );
}

// ─── Pieces ────────────────────────────────────────────────────────────────────────────────────

type HeadingRef = RefObject<HTMLHeadingElement | null>;

function Heading({ heading, children }: { heading: HeadingRef; children: ReactNode }) {
  return (
    <h1 ref={heading} tabIndex={-1} className="text-lg font-semibold text-ink outline-none">
      {children}
    </h1>
  );
}

function Footer({ children }: { children: ReactNode }) {
  return <div className="mt-6 flex items-center justify-between gap-2 border-t border-line pt-4">{children}</div>;
}

function Status({ children }: { children: ReactNode }) {
  return (
    <p role="status" className="mt-4 text-sm text-ink-2">
      {children}
    </p>
  );
}

function Alert({ children }: { children: ReactNode }) {
  return (
    <p role="alert" className="mt-4 text-sm text-risk">
      {children}
    </p>
  );
}

const INPUT = 'h-8 rounded-sm border bg-card px-2.5 text-sm text-ink outline-none focus-visible:border-accent';

// ─── 1. Host ───────────────────────────────────────────────────────────────────────────────────

function HostStep({
  remote,
  state,
  patch,
  heading,
  onCancel,
  onContinue,
}: {
  remote: RemoteGateway;
  state: State;
  patch(update: Partial<State>): void;
  heading: HeadingRef;
  onCancel(): void;
  onContinue(host: string, target?: WslTarget): void;
}) {
  const api = useMemo(() => createHubOnboardingApi({ remote }), [remote]);
  const [hosts, setHosts] = useState<DiscoveredHost[] | null>(null);
  const [distros, setDistros] = useState<WslDistro[] | null>(null);
  const [wslError, setWslError] = useState<string | null>(null);
  const [listError, setListError] = useState<string | null>(null);
  const typedId = useId();
  const errorId = useId();

  useEffect(() => {
    let live = true;
    remote.wslDistros?.().then(
      (found) => { if (live) setDistros(found.available ? found.distros : null); },
      (error: unknown) => { if (live) setWslError(messageOf(error)); },
    );
    api.discoverHosts().then(
      (found) => live && setHosts(found.filter((h) => h.kind === 'ssh')),
      (error: unknown) => {
        if (!live) return;
        setHosts([]);
        setListError(messageOf(error));
      },
    );
    return () => {
      live = false;
    };
  }, [api, remote]);

  const chosen = state.source === 'typed' ? state.typed.trim() : state.picked;

  return (
    <form
      onSubmit={(event) => {
        event.preventDefault();
        const distro = state.source === 'wsl' ? distros?.find((d) => d.name === chosen) : undefined;
        const problem = state.source === 'wsl'
          ? distro?.version === 2 ? undefined : 'Select a WSL2 distribution. WSL1 is unsupported.'
          : checkHost(chosen);
        if (problem !== undefined) {
          patch({ hostError: problem });
          return;
        }
        patch({ hostError: undefined });
        onContinue(chosen, distro === undefined ? undefined : { kind: 'wsl', distro: distro.name });
      }}
    >
      <Heading heading={heading}>Connect a remote machine</Heading>
      <p className="mt-2 text-sm text-ink-2">
        A server, a login node, or a SLURM cluster: PitCrew reaches it over your own SSH. Pick a host from your ssh
        config, or type one.
      </p>

      {wslError !== null && <p role="status">Could not list WSL distributions ({wslError}).</p>}
      {distros !== null && (
        <fieldset className="mt-4 flex flex-col gap-2">
          <legend>A WSL distro on this computer</legend>
          {distros.length === 0 && <p>No distributions are registered.</p>}
          {distros.map((distro) => (
            <label key={distro.name} className="flex gap-2 text-sm">
              <input type="radio" name="machine" disabled={distro.version !== 2}
                checked={state.source === 'wsl' && state.picked === distro.name}
                onChange={() => patch({ picked: distro.name, source: 'wsl', hostError: undefined })} />
              {distro.name}{distro.default ? ' (default)' : ''} — {distro.version !== 2 ? 'WSL1 unsupported' : distro.running ? 'Running' : 'Stopped; starts when probed'}
            </label>
          ))}
        </fieldset>
      )}
      <fieldset className="mt-4 flex flex-col gap-1.5">
        <legend className="mb-1.5 text-sm font-medium text-ink">Hosts in your ssh config</legend>
        {hosts === null && <p className="text-xs text-ink-2">Reading your ssh config…</p>}
        {listError !== null && <p className="text-xs text-ink-2">Could not read your ssh config ({listError}).</p>}
        {hosts !== null && hosts.length === 0 && listError === null && (
          <p className="text-xs text-ink-2">No hosts found there; type one below.</p>
        )}
        {hosts?.map((host) => (
          <label
            key={host.id}
            className={cx(
              'flex cursor-pointer items-center gap-2 rounded-sm border px-3 py-2 text-sm',
              state.source === 'list' && state.picked === host.id ? 'border-accent bg-accent-soft text-accent-text' : 'border-line hover:bg-hover',
            )}
          >
            <input
              type="radio"
              name="host"
              value={host.id}
              checked={state.source === 'list' && state.picked === host.id}
              onChange={() => patch({ picked: host.id, source: 'list', hostError: undefined })}
            />
            <span className="font-mono">{host.id}</span>
          </label>
        ))}
      </fieldset>

      <div className="mt-4 flex flex-col gap-1.5">
        <label htmlFor={typedId} className="text-sm font-medium text-ink">
          Or type a host
        </label>
        <input
          id={typedId}
          type="text"
          autoComplete="off"
          spellCheck={false}
          placeholder="user@server.example.org"
          value={state.typed}
          aria-invalid={state.hostError !== undefined && state.source === 'typed'}
          aria-describedby={state.hostError === undefined ? undefined : errorId}
          onChange={(event) => patch({ typed: event.target.value, source: 'typed', hostError: undefined })}
          className={cx(INPUT, 'font-mono', state.hostError === undefined ? 'border-line-2' : 'border-risk')}
        />
      </div>
      {state.hostError !== undefined && (
        <p id={errorId} role="alert" className="mt-2 text-sm text-risk">
          {state.hostError}
        </p>
      )}

      <Footer>
        <Button variant="ghost" onClick={onCancel}>
          Cancel
        </Button>
        <Button type="submit" variant="primary">
          Continue
        </Button>
      </Footer>
    </form>
  );
}

// ─── 2. Probe ──────────────────────────────────────────────────────────────────────────────────

function ProbeStep({
  state,
  heading,
  onBack,
  onRetry,
  onContinue,
  onCancel,
}: {
  state: State;
  heading: HeadingRef;
  onBack(): void;
  onRetry(): void;
  onContinue(): void;
  onCancel(): void;
}) {
  const { probe, probeError, host } = state;
  return (
    <div>
      <Heading heading={heading}>Checking {host}</Heading>
      <p className="mt-2 text-sm text-ink-2">Looking around without changing anything. SSH may ask you something first.</p>
      {probe === undefined && probeError === undefined && <Status>Connecting to {host}…</Status>}
      {probeError !== undefined && <Alert>Could not check {host}: {probeError}</Alert>}
      {probe !== undefined && (
        <dl className="mt-4 grid grid-cols-[max-content_1fr] gap-x-6 gap-y-2 text-sm" data-testid="probe">
          <dt className="text-ink-2">System</dt>
          <dd>
            {probe.os} {probe.arch}
          </dd>
          <dt className="text-ink-2">PitCrew</dt>
          <dd>
            {probe.helper === undefined
              ? 'Not installed yet'
              : `${probe.helper.version}, ${probe.helper.running ? 'running' : 'not running'}`}
          </dd>
          <dt className="text-ink-2">SLURM</dt>
          <dd>
            {probe.slurm === undefined
              ? 'Not found'
              : `${probe.slurm.version}${probe.slurm.defaultPartition === undefined ? '' : `, default partition ${probe.slurm.defaultPartition}`}`}
          </dd>
          <dt className="text-ink-2">tmux</dt>
          <dd>{probe.tmux === undefined ? 'Not known' : probe.tmux.version}</dd>
        </dl>
      )}
      <Footer>
        <span className="flex gap-2">
          <Button variant="ghost" onClick={onBack}>
            Back
          </Button>
          <Button variant="ghost" onClick={onCancel}>
            Cancel
          </Button>
        </span>
        <span className="flex gap-2">
          {probeError !== undefined && <Button onClick={onRetry}>Try again</Button>}
          <Button variant="primary" disabled={probe === undefined} onClick={onContinue}>
            Continue
          </Button>
        </span>
      </Footer>
    </div>
  );
}

// ─── 3. Launcher ───────────────────────────────────────────────────────────────────────────────

function LauncherStep({
  state,
  patch,
  heading,
  onBack,
  onCancel,
  onContinue,
}: {
  state: State;
  patch(update: Partial<State>): void;
  heading: HeadingRef;
  onBack(): void;
  onCancel(): void;
  onContinue(): void;
}) {
  const slurm = state.probe?.slurm !== undefined;
  const tmux = tmuxUsable(state.probe);
  const reasonId = useId();
  // Why a launcher is off, or what to know before picking it.
  const notes: Record<RemoteLauncher, { disabled: boolean; note?: string | undefined }> = {
    direct: { disabled: false },
    tmux:
      tmux === false
        ? { disabled: true, note: `tmux ${state.probe?.tmux?.version ?? ''} is too old here: PitCrew needs 3.2 or newer.` }
        : tmux === undefined
          ? { disabled: false, note: `Whether ${state.host} has tmux 3.2 or newer is not known: the plan says so if not.` }
          : { disabled: false },
    slurm: slurm ? { disabled: false } : { disabled: true, note: `SLURM was not found on ${state.host}.` },
  };
  return (
    <form
      onSubmit={(event) => {
        event.preventDefault();
        if (state.launcher === 'slurm') {
          const errors: Partial<Record<keyof JobDraft, string>> = {};
          for (const { key, label } of JOB_FIELDS) {
            const problem = key === 'cpus' ? checkCpus(state.job.cpus) : checkJobText(state.job[key], label.toLowerCase());
            if (problem !== undefined) errors[key] = problem;
          }
          patch({ jobErrors: errors });
          if (Object.keys(errors).length > 0) return;
        }
        onContinue();
      }}
    >
      <Heading heading={heading}>How PitCrew runs on {state.host}</Heading>
      <fieldset className="mt-4 flex flex-col gap-1.5">
        <legend className="mb-1.5 text-sm font-medium text-ink">Launcher</legend>
        {LAUNCHER_ORDER.map((launcher) => {
          const { disabled, note } = notes[launcher];
          const noteId = `${reasonId}-${launcher}`;
          return (
            <label
              key={launcher}
              className={cx(
                'flex items-start gap-2 rounded-sm border px-3 py-2 text-sm',
                disabled ? 'cursor-not-allowed border-line opacity-60' : 'cursor-pointer',
                !disabled && state.launcher === launcher ? 'border-accent bg-accent-soft text-accent-text' : 'border-line',
              )}
            >
              <input
                type="radio"
                name="launcher"
                value={launcher}
                disabled={disabled}
                aria-describedby={note === undefined ? undefined : noteId}
                checked={state.launcher === launcher}
                onChange={() => patch({ launcher })}
                className="mt-0.5"
              />
              <span>
                <span className="block font-medium">{LAUNCHERS[launcher].label}</span>
                <span className="block text-xs text-ink-2">{LAUNCHERS[launcher].hint}</span>
                {note !== undefined && (
                  <span id={noteId} className="block text-xs text-ink-2">
                    {note}
                  </span>
                )}
              </span>
            </label>
          );
        })}
      </fieldset>

      {state.launcher === 'slurm' && (
        <fieldset className="mt-4 grid grid-cols-2 gap-3">
          <legend className="mb-1.5 text-sm font-medium text-ink">The job</legend>
          {JOB_FIELDS.map(({ key, label, hint }) => (
            <JobField
              key={key}
              label={label}
              hint={hint}
              value={state.job[key]}
              error={state.jobErrors[key]}
              numeric={key === 'cpus'}
              onChange={(value) =>
                patch({ job: { ...state.job, [key]: value }, jobErrors: { ...state.jobErrors, [key]: undefined } })
              }
            />
          ))}
        </fieldset>
      )}

      <Footer>
        <span className="flex gap-2">
          <Button variant="ghost" onClick={onBack}>
            Back
          </Button>
          <Button variant="ghost" onClick={onCancel}>
            Cancel
          </Button>
        </span>
        <Button type="submit" variant="primary">
          Review the plan
        </Button>
      </Footer>
    </form>
  );
}

function JobField({
  label,
  hint,
  value,
  error,
  numeric,
  onChange,
}: {
  label: string;
  hint: string | undefined;
  value: string;
  error: string | undefined;
  numeric: boolean;
  onChange(value: string): void;
}) {
  const id = useId();
  const described = [hint === undefined ? undefined : `${id}-hint`, error === undefined ? undefined : `${id}-error`]
    .filter((x) => x !== undefined)
    .join(' ');
  return (
    <div className="flex flex-col gap-1">
      <label htmlFor={id} className="text-sm text-ink">
        {label}
      </label>
      <input
        id={id}
        type="text"
        inputMode={numeric ? 'numeric' : undefined}
        autoComplete="off"
        spellCheck={false}
        value={value}
        aria-invalid={error !== undefined}
        aria-describedby={described === '' ? undefined : described}
        onChange={(event) => onChange(event.target.value)}
        className={cx(INPUT, 'font-mono', error === undefined ? 'border-line-2' : 'border-risk')}
      />
      {hint !== undefined && (
        <p id={`${id}-hint`} className="text-xs text-ink-2">
          {hint}
        </p>
      )}
      {error !== undefined && (
        <p id={`${id}-error`} role="alert" className="text-xs text-risk">
          {error}
        </p>
      )}
    </div>
  );
}

// ─── 4. Review ─────────────────────────────────────────────────────────────────────────────────

function ReviewStep({
  state,
  heading,
  onBack,
  onRetry,
  onCancel,
  onConnect,
}: {
  state: State;
  heading: HeadingRef;
  onBack(): void;
  onRetry(): void;
  onCancel(): void;
  onConnect(plan: RemotePlan): void;
}) {
  const { plan, planError, notice } = state;
  const hazards = plan?.jobScript === undefined ? [] : scriptHazards(plan.jobScript);
  return (
    <div>
      <Heading heading={heading}>Review: connect {state.host}</Heading>
      {notice !== undefined && (
        <p role="status" className="mt-3 rounded-sm border border-warn bg-warn-soft px-3 py-2 text-sm text-ink">
          {notice}
        </p>
      )}
      {plan === undefined && planError === undefined && <Status>Working out the plan…</Status>}
      {planError !== undefined && <Alert>Could not make a plan: {planError}</Alert>}
      {plan !== undefined && (
        <>
          <h2 className="mt-4 text-sm font-medium text-ink">What will happen</h2>
          <ol className="mt-2 flex list-decimal flex-col gap-1 pl-5 text-sm text-ink" data-testid="plan-steps">
            {plan.steps.map((step, i) => (
              <li key={`${i}-${step}`}>{step}</li>
            ))}
          </ol>
          {plan.jobScript !== undefined && (
            <>
              <h2 className="mt-4 text-sm font-medium text-ink">The job script, exactly as it will be submitted</h2>
              <pre
                data-testid="job-script"
                tabIndex={0}
                aria-label="The job script"
                className="mt-2 max-h-80 overflow-auto rounded-sm border border-line bg-sunken p-3 font-mono text-xs whitespace-pre text-ink"
              >
                {plan.jobScript}
              </pre>
              {hazards.length > 0 && (
                <p role="alert" className="mt-2 rounded-sm border border-risk bg-risk-soft px-3 py-2 text-sm text-ink">
                  This script contains {hazards.join(', ')}: what you see above may not be what runs. Do not connect
                  unless you know why they are there.
                </p>
              )}
            </>
          )}
          <p className="mt-4 text-sm font-medium text-ink">Nothing changes on the remote until you press Connect.</p>
        </>
      )}
      <Footer>
        <span className="flex gap-2">
          <Button variant="ghost" onClick={onBack}>
            Back
          </Button>
          <Button variant="ghost" onClick={onCancel}>
            Cancel
          </Button>
        </span>
        <span className="flex gap-2">
          {planError !== undefined && <Button onClick={onRetry}>Try again</Button>}
          <Button variant="primary" disabled={plan === undefined} onClick={() => plan !== undefined && onConnect(plan)}>
            Connect
          </Button>
        </span>
      </Footer>
    </div>
  );
}

// ─── 5. Connect ────────────────────────────────────────────────────────────────────────────────

const PROGRESS_LABEL: Record<RemoteProgress['state'] | 'waiting', string> = {
  waiting: 'Waiting',
  running: 'Running…',
  done: 'Done',
  failed: 'Failed',
};

function ConnectStep({
  state,
  heading,
  onReview,
  onCancel,
  onLeave,
  onStop,
}: {
  state: State;
  heading: HeadingRef;
  onReview(): void;
  onCancel(): void;
  onLeave(): void;
  /** Stops the add (asked for, and confirmed). */
  onStop(): void;
}) {
  const { progress, failure } = state;
  const [confirming, setConfirming] = useState(false);
  // Worked out from the progress as it is now: the step that failed, with its detail, else the
  // whole add's ("add"), else the error's.
  const failedStep = [...progress].reverse().find((p) => p.state === 'failed' && p.step !== ADD);
  const whole = progress.find((p) => p.step === ADD);
  const failedDetail = failedStep?.detail ?? whole?.detail ?? failure?.message;
  // The plan's steps, in its order, each with its latest message; then any step it did not name.
  const planned = state.plan?.steps ?? [];
  const extra = progress.filter((p) => p.step !== ADD && !planned.includes(p.step)).map((p) => p.step);
  const rows = [...planned, ...extra].map(
    (step) => progress.find((p) => p.step === step) ?? { step, state: 'waiting' as const, detail: undefined },
  );
  return (
    <div>
      <Heading heading={heading}>Connecting {state.host}</Heading>
      {failure === undefined && (
        <Status>
          Working. If SSH asks for a password, a code or a host key, answer in the dialog.
          {state.launcher === 'slurm' && ' A SLURM job may wait in the queue for several minutes; PitCrew waits with it.'}
        </Status>
      )}
      <ol className="mt-4 flex flex-col gap-1.5 text-sm" aria-label="Progress" data-testid="progress">
        {rows.map((p) => (
          <li key={p.step} className="flex flex-col rounded-sm border border-line px-3 py-2">
            <span className="flex items-center justify-between gap-3">
              <span className="text-ink">{p.step}</span>
              <span className={cx('text-xs', p.state === 'failed' ? 'text-risk' : p.state === 'done' ? 'text-ok' : 'text-ink-2')}>
                {PROGRESS_LABEL[p.state]}
              </span>
            </span>
            {p.detail !== undefined && <span className="mt-1 text-xs whitespace-pre-wrap text-ink-2">{p.detail}</span>}
          </li>
        ))}
      </ol>
      {failure !== undefined && (
        <Alert>
          {failure.cancelled
            ? 'Connecting was stopped'
            : failedStep === undefined
              ? 'Connecting failed'
              : `Connecting failed at “${failedStep.step}”`}
          : {failedDetail}
        </Alert>
      )}
      {failure === undefined && state.cancelling === true && <Status>Stopping, and undoing what was started…</Status>}
      {state.cancelError !== undefined && <Alert>{state.cancelError}</Alert>}
      {failure !== undefined ? (
        <Footer>
          <Button variant="ghost" onClick={onCancel}>
            Close
          </Button>
          <Button variant="primary" onClick={onReview}>
            Back to review
          </Button>
        </Footer>
      ) : (
        <Footer>
          <Button variant="ghost" disabled={state.cancelling === true} onClick={() => setConfirming(true)}>
            Stop connecting…
          </Button>
          <Button onClick={onLeave}>Leave it running</Button>
        </Footer>
      )}
      {failure === undefined && (
        <p className="mt-2 text-xs text-ink-2">
          Leave it running to go back to PitCrew: the connection goes on, and the workspace shows up in the switcher
          when it is ready.
        </p>
      )}
      {confirming && (
        <Dialog open onOpenChange={(open) => !open && setConfirming(false)}>
          <DialogContent
            title={`Stop connecting ${state.host}?`}
            description="PitCrew stops what it started there, such as the helper or a submitted job, and connects nothing."
          >
            <DialogFooter>
              <Button onClick={() => setConfirming(false)}>Keep connecting</Button>
              <Button
                variant="primary"
                onClick={() => {
                  setConfirming(false);
                  onStop();
                }}
              >
                Stop connecting
              </Button>
            </DialogFooter>
          </DialogContent>
        </Dialog>
      )}
    </div>
  );
}

// ─── 6. Setup ──────────────────────────────────────────────────────────────────────────────────

function SetupStep({
  state,
  patch,
  heading,
  workspace,
  onDone,
  onLater,
}: {
  state: State;
  patch(update: Partial<State>): void;
  heading: HeadingRef;
  workspace: GatewayWorkspace;
  onDone(): void;
  /** Leaves it for now: opening the workspace later leads to its first run. */
  onLater(): void;
}) {
  return (
    <div>
      <Heading heading={heading}>Set up {state.host}</Heading>
      <WorkspaceScope
        ws={workspace.id}
        fallback={(reason) =>
          reason.kind === 'failed' ? (
            <Alert>Could not list the workspaces: {reason.message}</Alert>
          ) : (
            <Status>Waiting for {workspace.name} to appear…</Status>
          )
        }
      >
        <RemoteSetup state={state} patch={patch} onDone={onDone} />
      </WorkspaceScope>
      {/* Also while the new workspace is still on its way. */}
      <p className="mt-6 text-xs text-ink-2">
        <button type="button" onClick={onLater} className="font-medium text-accent-text underline underline-offset-2">
          Set it up later
        </button>
        : PitCrew asks again when you open {workspace.name}.
      </p>
    </div>
  );
}

/** In the new workspace's data scope: sets it up through its own gateway transport, if it needs it. */
function RemoteSetup({
  state,
  patch,
  onDone,
}: {
  state: State;
  patch(update: Partial<State>): void;
  onDone(): void;
}) {
  const info = useWorkspace().data;
  const gateway = useGatewayWorkspace();
  const setUp = useSetUp();
  const api = useMemo(() => createHubOnboardingApi({ setUp }), [setUp]);
  const needed = info?.setup_needed === true;

  // Already set up (a hub someone set up before): nothing to ask.
  useEffect(() => {
    if (info !== undefined && !needed) onDone();
  }, [info, needed, onDone]);

  if (info === undefined) {
    return (
      <Status>
        Connecting to {gateway?.name ?? state.host}…
        {gateway?.state === 'unreachable' && gateway.detail !== undefined ? ` (${gateway.detail})` : ''}
      </Status>
    );
  }
  if (!needed) return <Status>{info.workspace.name} is already set up.</Status>;
  return (
    <>
      <p className="mt-2 mb-4 text-sm text-ink-2">
        The PitCrew on {state.host} is new: name its workspace and say who you are there.
      </p>
      <SetupForm
        values={state.setup}
        handleEdited={state.handleEdited}
        onChange={(setup, handleEdited) => patch({ setup, handleEdited })}
        submit={(input) => api.setupWorkspace(input)}
        onDone={onDone}
        onAlreadySetUp={onDone}
        machineLabel={`${state.host}’s name`}
        footer={(busy) => (
          <Footer>
            <span />
            <Button type="submit" variant="primary" disabled={busy}>
              {busy ? 'Working…' : 'Set up'}
            </Button>
          </Footer>
        )}
      />
    </>
  );
}

// ─── 7. Done ───────────────────────────────────────────────────────────────────────────────────

function DoneStep({
  host,
  workspace,
  heading,
  onOpen,
}: {
  host: string;
  workspace: GatewayWorkspace;
  heading: HeadingRef;
  onOpen(workspace: GatewayWorkspace): void;
}) {
  return (
    <div>
      <Heading heading={heading}>Connected</Heading>
      <p className="mt-2 text-sm text-ink-2">
        {host} is connected, as the workspace “{workspace.name}”.
      </p>
      <div className="mt-6">
        <Button variant="primary" onClick={() => onOpen(workspace)}>
          Open {workspace.name}
        </Button>
      </div>
    </div>
  );
}
