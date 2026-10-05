// An in-memory `OnboardingApi` that behaves plausibly: it "detects" a few missing tools, streams
// progress over short timers, and fabricates scan suggestions from synthetic data (never real
// transcripts, host names or tokens). The UI is built against `OnboardingApi`, not this file, so a
// real client can replace it without touching a step component.
//
// For tests, and for a development build's `?onboarding=fake` (`first-run-page.tsx`). A production
// build never loads it: nothing outside tests imports it except behind `import.meta.env.DEV`.

import type {
  AgentAccount,
  CheckRowId,
  CreateFromScanResult,
  DiscoveredHost,
  HooksDiff,
  ImportDryRunResult,
  ImportFilter,
  ImportResult,
  InstallHelperOptions,
  InstallProgressEvent,
  IntegrationStatus,
  LauncherOption,
  MachineCheckResult,
  MachineCheckRow,
  MachineTarget,
  OnboardingApi,
  ProjectSelection,
  ScanProgressEvent,
  ScanResult,
  ScanTarget,
  SetupWorkspaceInput,
  SetupWorkspaceResult,
  StartSignInResult,
  Streamed,
} from './api.ts';
import { machineTargetLabel, SetupRefused, targetKey } from './api.ts';
import { checkSetup, type SetupField } from './validation.ts';

function wait(ms: number): Promise<void> {
  return new Promise((resolve) => setTimeout(resolve, ms));
}

/** Runs `steps`, one per tick, calling `onEvent` for each; returns a `Streamed` that can cancel. */
function streamSteps<T>(steps: { delay: number; event: T }[], onEvent: (event: T) => void): Streamed {
  let cancelled = false;
  const timers: ReturnType<typeof setTimeout>[] = [];
  let elapsed = 0;
  for (const step of steps) {
    elapsed += step.delay;
    timers.push(
      setTimeout(() => {
        if (!cancelled) onEvent(step.event);
      }, elapsed),
    );
  }
  return {
    cancel() {
      cancelled = true;
      for (const timer of timers) clearTimeout(timer);
    },
  };
}

const SLURM_SCRIPT = (host: string) => `#!/bin/bash
#SBATCH --job-name=pitcrewd
#SBATCH --time=7-00:00:00
#SBATCH --cpus-per-task=1
#SBATCH --mem=256M
#SBATCH --output=%x-%j.log

# Self-renewing: pitcrewd re-submits itself before the wall-clock limit expires, so the runner
# stays up across the cluster's job-length cap. Deployed to ~/.pitcrew/bin/<version>/ on ${host}.
exec "$HOME/.pitcrew/bin/current/pitcrewd" runner --renew
`;

function baseRows(): MachineCheckRow[] {
  return [
    { id: 'cli-claude', label: 'Claude Code CLI', status: 'ok', detail: '2.1.4', fixable: false },
    { id: 'cli-codex', label: 'Codex CLI', status: 'ok', detail: '0.44.0', fixable: false },
    { id: 'cli-opencode', label: 'OpenCode CLI', status: 'missing', detail: 'not on PATH', fixable: true, fix: 'install-page' },
    { id: 'tmux', label: 'tmux', status: 'ok', detail: '3.4', fixable: false },
    { id: 'git', label: 'git', status: 'ok', detail: '2.45.2', fixable: false },
    { id: 'gh', label: 'GitHub CLI (gh)', status: 'missing', detail: 'not on PATH', fixable: true, fix: 'install-page' },
    { id: 'disk', label: 'Disk space', status: 'ok', detail: '128 GB free', fixable: false },
  ];
}

function rowsFor(target: MachineTarget): MachineCheckRow[] {
  const rows = baseRows();
  if (target.kind === 'ssh') rows.push({ id: 'slurm', label: 'SLURM', status: 'ok', detail: 'slurm 23.02', fixable: false });
  return rows;
}

function suggestedScan(): ScanResult {
  return {
    counts: {
      byEngine: { claude: 41, codex: 12, opencode: 3 },
      byFolder: [
        { path: '~/code/diffusion-study', count: 28 },
        { path: '~/code/pitcrew', count: 19 },
        { path: '~/code/scratch', count: 9 },
      ],
      byMonth: [
        { month: '2026-09', count: 22 },
        { month: '2026-08', count: 24 },
        { month: '2026-07', count: 10 },
      ],
    },
    suggestedProjects: [
      {
        id: 'sp-diffusion',
        name: 'Diffusion study',
        path: '~/code/diffusion-study',
        workstreams: [
          { id: 'sw-seed-runs', name: 'Seed runs', branch: 'main', sessionCount: 17 },
          { id: 'sw-eval', name: 'Evaluation', branch: 'eval', sessionCount: 11 },
        ],
      },
      {
        id: 'sp-pitcrew',
        name: 'PitCrew',
        path: '~/code/pitcrew',
        workstreams: [{ id: 'sw-onboarding', name: 'Onboarding wizard', branch: 's/O', sessionCount: 19 }],
      },
      {
        id: 'sp-scratch',
        name: 'Scratch',
        path: '~/code/scratch',
        workstreams: [{ id: 'sw-misc', name: 'Misc experiments', sessionCount: 9 }],
      },
    ],
  };
}

function dryRunCount(filter: ImportFilter): number {
  if (filter.mode === 'none') return 0;
  const all = 56;
  if (filter.mode === 'all') return all;
  let count = all;
  if (filter.engines !== undefined) count = Math.round((count * filter.engines.length) / 3);
  if (filter.since !== undefined) count = Math.round(count * 0.6);
  if (filter.folders !== undefined && filter.folders.length > 0) {
    count = Math.round((count * filter.folders.length) / 3);
  }
  return count;
}

export interface FakeOnboardingApiOptions {
  /** Scales every delay; tests pass 0 to run instantly. Default 1. */
  speed?: number;
}

export function createFakeOnboardingApi(options: FakeOnboardingApiOptions = {}): OnboardingApi {
  const speed = options.speed ?? 1;
  const delay = (ms: number) => Math.round(ms * speed);
  const checks = new Map<string, MachineCheckRow[]>();
  const accounts = new Map<string, AgentAccount>([
    ['claude', { engine: 'claude', installed: true, signedIn: true, account: 'sam@example.com' }],
    ['codex', { engine: 'codex', installed: true, signedIn: false }],
    ['opencode', { engine: 'opencode', installed: true, signedIn: false }],
  ]);
  /** Sign-ins whose "login" still runs, by engine: each ends a moment after it starts. */
  const running = new Set<string>();
  /** Each running sign-in's end, which `stopSignIn` calls off. */
  const endings = new Map<string, ReturnType<typeof setTimeout>>();
  const integrations = new Map<string, IntegrationStatus>([
    ['github', { id: 'github', connected: true, detail: 'sam' }],
    ['jira', { id: 'jira', connected: false }],
    ['linear', { id: 'linear', connected: false }],
    ['gitlab', { id: 'gitlab', connected: false }],
  ]);
  let nextSignIn = 0;
  let setUp = false;

  return {
    unavailable: new Set(),

    async discoverHosts(): Promise<DiscoveredHost[]> {
      await wait(delay(150));
      return [
        { kind: 'wsl', id: 'Ubuntu-22.04', detail: 'default' },
        { kind: 'ssh', id: 'hpc-login', detail: 'login.hpc.example.edu' },
        { kind: 'ssh', id: 'staging', detail: 'staging.internal' },
      ];
    },

    async setupWorkspace(input: SetupWorkspaceInput): Promise<SetupWorkspaceResult> {
      await wait(delay(100));
      // The hub's own checks, so the fake refuses what the hub would.
      const errors = checkSetup({
        workspaceName: input.workspaceName,
        personName: input.person.name,
        handle: input.person.handle,
        machineName: input.machineName,
      });
      const [field, message] = Object.entries(errors)[0] ?? [];
      if (message !== undefined) throw new SetupRefused(message, { field: field as SetupField });
      if (setUp) throw new SetupRefused('This workspace is already set up.', { alreadySetUp: true });
      setUp = true;
      return {
        workspace: { id: 'ws-local', name: input.workspaceName.trim() },
        me: { name: input.person.name.trim(), handle: input.person.handle },
      };
    },

    async checkMachine(target: MachineTarget): Promise<MachineCheckResult> {
      await wait(delay(400));
      const key = targetKey(target);
      const rows = checks.get(key) ?? rowsFor(target);
      checks.set(key, rows);
      return { machine: target, rows };
    },

    // The fake installs the helper anywhere, so its every step can be tried on this computer.
    needsHelper: () => true,

    async fixMachineRow(target: MachineTarget, row: CheckRowId): Promise<MachineCheckRow> {
      const key = targetKey(target);
      const rows = checks.get(key) ?? rowsFor(target);
      const found = rows.find((r) => r.id === row);
      if (found === undefined) throw new Error(`No check row "${row}"`);
      if (!found.fixable) throw new Error(`"${found.label}" cannot be fixed automatically`);
      await wait(delay(500));
      // A real fix opens the tool's install page; the fake pretends the person installed it.
      const fixed: MachineCheckRow =
        row === 'cli-opencode'
          ? { ...found, status: 'ok', detail: '0.9.2 (installed)', fixable: false, fix: undefined }
          : { ...found, status: 'ok', detail: 'gh version 2.45.0 (installed)', fixable: false, fix: undefined };
      const next = rows.map((r) => (r.id === row ? fixed : r));
      checks.set(key, next);
      return fixed;
    },

    async launcherOptions(target: MachineTarget): Promise<LauncherOption[]> {
      await wait(delay(100));
      const options: LauncherOption[] = [
        { launcher: 'direct', recommended: target.kind === 'local' },
        { launcher: 'tmux', recommended: target.kind !== 'local' },
        {
          launcher: 'systemd-user',
          recommended: false,
          ...(target.kind === 'local' ? {} : { unavailable: 'No systemd user session on this host' }),
        },
        {
          launcher: 'slurm',
          recommended: false,
          ...(target.kind === 'ssh' ? {} : { unavailable: 'SLURM is only offered for SSH hosts' }),
        },
      ];
      return options;
    },

    streamInstallHelper(
      options: InstallHelperOptions,
      onEvent: (event: InstallProgressEvent) => void,
    ): Streamed {
      const label = machineTargetLabel(options.machine);
      const steps: { delay: number; event: InstallProgressEvent }[] = [];
      if (options.launcher === 'slurm') {
        steps.push({ delay: delay(150), event: { type: 'script-preview', script: SLURM_SCRIPT(label) } });
      }
      steps.push(
        { delay: delay(200), event: { type: 'log', line: `Connecting to ${label}…` } },
        { delay: delay(250), event: { type: 'log', line: 'Checking sha256 of pitcrewd-x86_64-unknown-linux-musl…' } },
        { delay: delay(250), event: { type: 'log', line: `Uploading to ~/.pitcrew/bin/0.1.0/ on ${label}…` } },
        { delay: delay(250), event: { type: 'log', line: 'Running pitcrewd --version…' } },
        {
          delay: delay(200),
          event: { type: 'log', line: `Switching the current symlink, launcher: ${options.launcher}.` },
        },
        { delay: delay(150), event: { type: 'done' } },
      );
      return streamSteps(steps, onEvent);
    },

    async agentAccounts(): Promise<AgentAccount[]> {
      await wait(delay(100));
      return [...accounts.values()];
    },

    async startSignIn(engine, machine): Promise<StartSignInResult> {
      await wait(delay(150));
      nextSignIn += 1;
      const id = `onboarding-signin-${engine}-${targetKey(machine)}-${nextSignIn}`;
      // A real sign-in completes when the CLI's own login exits; the fake's "login" ends a moment
      // after it starts, and its CLI then says it is signed in.
      running.add(engine);
      clearTimeout(endings.get(engine));
      endings.set(
        engine,
        setTimeout(() => {
          running.delete(engine);
          endings.delete(engine);
          const current = accounts.get(engine);
          if (current !== undefined) accounts.set(engine, { ...current, signedIn: true, account: `${engine}@example.com` });
        }, delay(600)),
      );
      const command = { claude: ['claude', 'auth', 'login'], codex: ['codex', 'login'], opencode: ['opencode', 'auth', 'login'] };
      return { terminalSessionId: id, command: command[engine] };
    },

    async signInRunning(engine): Promise<boolean> {
      await wait(delay(50));
      return running.has(engine);
    },

    async stopSignIn(engine): Promise<void> {
      // Left before it finished: it ends now, and the CLI is not signed in.
      clearTimeout(endings.get(engine));
      endings.delete(engine);
      running.delete(engine);
      await wait(delay(20));
    },

    async integrationStatus(): Promise<IntegrationStatus[]> {
      await wait(delay(100));
      return [...integrations.values()];
    },

    streamScan(target: ScanTarget, onEvent: (event: ScanProgressEvent) => void): Streamed {
      const result = suggestedScan();
      // A remote machine has fewer local transcripts to walk than this computer, in the fake.
      const total = target.machine.kind === 'local' ? 56 : 40;
      const steps: { delay: number; event: ScanProgressEvent }[] = [
        { delay: delay(150), event: { type: 'progress', scanned: Math.round(total * 0.2), total } },
        { delay: delay(150), event: { type: 'progress', scanned: Math.round(total * 0.6), total } },
        { delay: delay(150), event: { type: 'progress', scanned: total, total } },
        { delay: delay(100), event: { type: 'done', result } },
      ];
      return streamSteps(steps, onEvent);
    },

    async createFromScan(selection: ProjectSelection[]): Promise<CreateFromScanResult> {
      await wait(delay(300));
      const projects: CreateFromScanResult['projects'] = selection.map((s, i) => ({
        id: `prj-${s.suggestionId}`,
        key: s.name.slice(0, 3).toUpperCase() || `PRJ${i}`,
        name: s.name,
        status: 'in_progress' as const,
        lead: 'mem-me',
        members: ['mem-me'],
        external: [],
      }));
      const workstreams: CreateFromScanResult['workstreams'] = selection.flatMap((s) =>
        s.workstreams.map((w) => ({
          id: `wst-${w.suggestionId}`,
          project: `prj-${s.suggestionId}`,
          name: w.name,
          status: 'active' as const,
          health: 'on_track' as const,
          locations: [],
          external: [],
        })),
      );
      return { projects, workstreams };
    },

    async importSessions(filter: ImportFilter): Promise<ImportDryRunResult> {
      await wait(delay(200));
      return { count: dryRunCount(filter) };
    },

    async commitImport(filter: ImportFilter): Promise<ImportResult> {
      await wait(delay(300));
      return { imported: dryRunCount(filter) };
    },

    async hooksDiff(): Promise<HooksDiff> {
      await wait(delay(150));
      return {
        revision: "synthetic-preview",
        engines: [{ engine: 'claude', status: 'missing', detail: 'Synthetic Claude hooks.' }, { engine: 'codex', status: 'missing', detail: 'Synthetic Codex hooks.' }],
        files: [
          {
            path: '~/.claude/settings.json',
            before: '{\n  "hooks": {}\n}\n',
            after: '{\n  "hooks": {\n    "PostToolUse": "pitcrew hook post-tool-use"\n  }\n}\n',
          },
          {
            path: '~/.codex/config.toml',
            before: null,
            after: '[hooks]\npost_tool_use = "pitcrew hook post-tool-use"\n',
          },
        ],
      };
    },

    async installHooks(): Promise<void> {
      await wait(delay(250));
    },

    async readSafety() {
      return { permissionMode: 'default' as const, backOfficeEnabled: false, backOfficeCaps: { maxAutoAcceptPerHour: 20 } };
    },
    async saveSafety(settings): Promise<void> {
      // A workspace that opts into the back office takes a touch longer, in the fake: it also
      // persists the auto-accept caps, not just the flag.
      await wait(delay(settings.backOfficeEnabled ? 200 : 150));
    },
  };
}
