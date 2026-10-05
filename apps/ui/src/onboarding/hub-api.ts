// The real `OnboardingApi`: what the hub and the desktop gateway can do today.
// - `setupWorkspace` is `POST /v1/setup`, through the data layer's `setUp` for the workspace in
//   view (so its cache knows at once that it is set up);
// - `discoverHosts` is the gateway's `sshHosts`, in the desktop app;
// - `checkMachine`, `fixMachineRow`, `agentAccounts`, `startSignIn`, `signInRunning` and
//   `stopSignIn` are machine setup's routes on the hub's own machine (api-v1.md, "Machine setup"),
//   through the workspace's transport (a remote workspace's goes through the gateway to its hub).
//   A fix only opens the tool's install page, from this app's own table (`install-pages.ts`), and
//   checks the row again: nothing is installed. Another machine (an SSH host, a WSL distro, an HPC
//   login node) is checked when it is connected, by the connect wizard over SSH: its check here
//   says so (`deferred`) rather than failing;
// - `launcherOptions` and `streamInstallHelper` have no backend here: installing the helper needs a
//   plan the person reviewed (the gateway's plan and add, with the SLURM script shown), which the
//   connect wizard makes itself, and the first run's machine is the hub's own, which needs none.
//   So the first run never offers its install step;
// - `streamScan` is `POST /v1/machines/{id}/scan` on the hub's own machine, and `createFromScan`
//   is `POST /v1/projects` and `POST /v1/workstreams` from that scan's suggestions, both through
//   the data layer's client for the workspace in view;
// - hooks preview/confirmation and workspace safety use device-only hub routes;
// - everything else has no backend yet: it is listed in `unavailable`, rejects if called, and
//   `stepsFor` leaves its step out. So the real first run is Welcome, Workspace, Machine check,
//   Sign in, Scan, Create, Import, Hooks, Safety, Done.
//
// The transports (the browser's `fetch`, the desktop gateway's `gateway_request`) hand over a whole
// body, so the scan's progress frames arrive together with its report, at its end; the scan step
// shows that it is scanning meanwhile. Progress as it happens needs a streaming request in the
// data layer and the gateway.

import {
  ApiError,
  SetupConflict,
  type Api,
  type Location,
  type Project,
  type RemoteGateway,
  type Setup,
  type SetupResult,
  type TransportResponse,
  type Transport,
  type Workstream,
} from '../data/index.ts';
import {
  SetupRefused,
  type CheckRowId,
  type CreateFromScanResult,
  type MachineTarget,
  type OnboardingApi,
  type OnboardingCall,
  type ProjectSelection,
  type ScanProgressEvent,
  type ScanTarget,
  type SignInMethod,
  type Streamed,
} from './api.ts';
import { installPage } from './install-pages.ts';
import { toAccounts, toCheckRows, toSignIn, toStartSignInResult, wireItem } from './machine-wire.ts';
import { projectKeyFor } from './project-key.ts';
import {
  parseScanFrames,
  toScanResult,
  type WireScanReport,
  type WireSuggestion,
  type WireWorkstreamSuggestion,
} from './scan-wire.ts';
import { fieldOfMessage } from './validation.ts';

const NOT_YET: readonly OnboardingCall[] = [
  'integrationStatus',
  // The connect wizard installs the helper itself, from a plan the person reviewed.
  'launcherOptions',
  'streamInstallHelper',
];

/** The calls that need the workspace's transport: machine setup's routes. */
const MACHINE_SETUP: readonly OnboardingCall[] = [
  'checkMachine',
  'fixMachineRow',
  'agentAccounts',
  'startSignIn',
  'signInRunning',
  'stopSignIn',
];

/** What a machine that is not the hub's own shows in place of its check: it is checked later. */
export const CHECKED_WHEN_CONNECTED =
  'Checked when you connect it: PitCrew checks this machine over SSH while connecting it, before anything is installed there.';

/** What the scan and create steps use of the data layer's client (`useApi()`). */
export type HubData = Pick<Api, 'transport' | 'machines' | 'projects' | 'createProject' | 'createWorkstream'>;

export interface HubOnboardingOptions {
  /** `POST /v1/setup` for the workspace in view (`useSetUp()`); none outside a workspace. */
  setUp?: ((setup: Setup) => Promise<SetupResult>) | undefined;
  /** The gateway's remote commands; `null` in a browser. */
  remote?: RemoteGateway | null | undefined;
  /** The workspace hub transport, including remote hubs; defaults to `data.transport`. */
  transport?: Transport | undefined;
  /** The workspace's client (`useApi()`), for the scan and creating from it; without it, neither. */
  data?: HubData | undefined;
  /** Client used only for hooks when remote scan/create are unavailable. */
  hooksData?: HubData | undefined;
  /**
   * Opens a page outside the app (a tool's install page, for a check row's fix). By default a new
   * browser tab with no way back to this one, in a browser; the desktop app opens no window, so
   * there the step shows the page to copy instead.
   */
  openPage?: ((url: string) => void) | undefined;
}

/** A new browsing context with no way back to this one. */
function openInBrowser(url: string): void {
  window.open(url, '_blank', 'noopener,noreferrer');
}

/** How many keys `createFromScan` tries for one project while the hub says each is taken. */
const KEY_ATTEMPTS = 5;

function unavailable(call: OnboardingCall): never {
  throw new Error(`${call} is not available yet.`);
}

function refused(error: unknown): unknown {
  if (error instanceof SetupConflict) {
    return error.alreadySetUp
      ? new SetupRefused(error.message, { alreadySetUp: true })
      : new SetupRefused(error.message, { field: 'handle' });
  }
  if (error instanceof ApiError && error.code === 'invalid') {
    return new SetupRefused(error.message, { field: fieldOfMessage(error.message) });
  }
  return error;
}

/** A refusal's message, from its `ApiError` body when it has one. */
function refusal(res: TransportResponse): string {
  try {
    const body: unknown = JSON.parse(res.body);
    if (typeof body === 'object' && body !== null) {
      const { message } = body as { message?: unknown };
      if (typeof message === 'string' && message !== '') return message;
    }
  } catch {
    // Not the contract's shape: say what is known.
  }
  return `The hub answered ${res.status}.`;
}

function messageOf(error: unknown): string {
  return error instanceof Error ? error.message : String(error);
}

/** The JSON body of a `200`/`201` answer, or the hub's refusal as an error. */
async function answer(transport: Transport, method: 'GET' | 'POST', path: string, body?: string): Promise<unknown> {
  const res = await transport.request(method, path, body);
  if (res.status !== 200 && res.status !== 201) throw new Error(refusal(res));
  try {
    return JSON.parse(res.body) as unknown;
  } catch {
    throw new Error(`The hub answered ${path} with something other than JSON.`);
  }
}

/** The hub's own machine, by `GET /v1/machines`: its first local one, as the hub picks it. */
async function hubMachine(transport: Transport): Promise<string> {
  const machines = await answer(transport, 'GET', '/v1/machines');
  const own = Array.isArray(machines)
    ? (machines as unknown[])
        .filter((m): m is { id?: unknown; kind?: unknown } => typeof m === 'object' && m !== null)
        .find((m) => m.kind === 'local')
    : undefined;
  if (typeof own?.id !== 'string') throw new Error('This hub has no machine of its own yet: set the workspace up first.');
  return own.id;
}

/** Machine setup acts on the hub's own machine; any other target is set up while connecting it. */
function onlyHubMachine(target: MachineTarget): void {
  if (target.kind !== 'local') {
    throw new Error('A remote machine is checked and signed into through its own workspace, once it is connected.');
  }
}

/** The hub's own machine: its first local one, as the hub itself picks it. */
async function ownMachine(data: HubData, signal: AbortSignal): Promise<string> {
  const machines = await data.machines(signal);
  const own = machines.find((m) => m.kind === 'local');
  if (own === undefined) throw new Error('This hub has no machine of its own to scan yet.');
  return own.id;
}

/** A suggested workstream, and the suggested project it came from. */
interface FoundWorkstream {
  project: WireSuggestion;
  workstream: WireWorkstreamSuggestion;
}

function findWorkstream(report: WireScanReport, id: string): FoundWorkstream | undefined {
  for (const project of report.suggestions) {
    const workstream = project.workstreams.find((w) => w.id === id);
    if (workstream !== undefined) return { project, workstream };
  }
  return undefined;
}

/**
 * Where a suggested workstream is: its folder (a folder suggestion's `id` is its path), or its
 * project's root on its branch. It keeps that place when the person moved it to another project.
 */
export function locationOf(machine: string, { project, workstream }: FoundWorkstream): Location {
  return workstream.branch === undefined
    ? { machine, path: workstream.id }
    : { machine, path: project.path, branch: workstream.branch };
}

/** The name the person gave, or the suggestion's when they cleared it. */
function nameOr(given: string, suggested: string): string {
  const trimmed = given.trim();
  return trimmed === '' ? suggested : trimmed;
}

/** The last scan that finished, which `createFromScan` builds on. */
interface LastScan {
  machine: string;
  report: WireScanReport;
}

export function createHubOnboardingApi(options: HubOnboardingOptions = {}): OnboardingApi {
  const { setUp, data } = options;
  const hooksData = options.hooksData ?? data;
  const remote = options.remote ?? null;
  const transport = options.transport ?? data?.transport ?? options.hooksData?.transport;
  const openPage = options.openPage ?? (transport?.kind === 'browser' ? openInBrowser : undefined);
  const missing = new Set<OnboardingCall>(NOT_YET);
  if (setUp === undefined) missing.add('setupWorkspace');
  if (remote === null) missing.add('discoverHosts');
  if (transport === undefined) {
    missing.add('importSessions');
    missing.add('commitImport');
    for (const call of MACHINE_SETUP) missing.add(call);
    missing.add('hooksDiff');
    missing.add('installHooks');
    missing.add('saveSafety');
    missing.add('readSafety');
  }
  if (hooksData === undefined) {
    missing.add('hooksDiff');
    missing.add('installHooks');
  }
  /** The workspace's transport, for machine setup's routes. */
  function hub(call: OnboardingCall): Transport {
    if (transport === undefined) return unavailable(call);
    return transport;
  }
  /** One row checked again, on the hub's own machine. */
  async function recheck(hubTransport: Transport, row: CheckRowId) {
    const machine = await hubMachine(hubTransport);
    const checked = toCheckRows(
      await answer(hubTransport, 'GET', `/v1/machines/${encodeURIComponent(machine)}/check?row=${wireItem(row)}`),
    );
    return checked.find((r) => r.id === row);
  }
  function signInPath(machine: string, engine: string): string {
    return `/v1/machines/${encodeURIComponent(machine)}/agents/${encodeURIComponent(engine)}/sign-in`;
  }
  if (data === undefined) {
    missing.add('streamScan');
    missing.add('createFromScan');
  }
  let lastScan: LastScan | undefined;
  // What `createFromScan` created in this run, so that trying again after a failure part-way
  // creates the rest rather than the first ones twice: projects by suggestion, workstreams by
  // project and suggestion.
  const createdProjects = new Map<string, Project>();
  const createdWorkstreams = new Map<string, Workstream>();

  async function createProject(hub: HubData, name: string, root: Location, taken: Set<string>): Promise<Project> {
    for (let attempt = 1; ; attempt += 1) {
      const key = projectKeyFor(name, taken);
      taken.add(key);
      try {
        return await hub.createProject({ key, name, root });
      } catch (error) {
        // `409` means the key: another client took it after the projects were read.
        if (!(error instanceof ApiError && error.code === 'conflict') || attempt >= KEY_ATTEMPTS) throw error;
      }
    }
  }

  return {
    unavailable: missing,

    async discoverHosts() {
      if (remote === null) return unavailable('discoverHosts');
      return (await remote.sshHosts()).map((host) => ({ kind: 'ssh' as const, id: host }));
    },

    async setupWorkspace(input) {
      if (setUp === undefined) return unavailable('setupWorkspace');
      let result: SetupResult;
      try {
        result = await setUp({
          workspace_name: input.workspaceName,
          person: { name: input.person.name, handle: input.person.handle },
          machine_name: input.machineName,
        });
      } catch (error) {
        throw refused(error);
      }
      return {
        workspace: { id: result.workspace.id, name: result.workspace.name },
        me: { name: result.me.name, handle: result.me.handle },
      };
    },

    needsHelper: (target) => target.kind !== 'local',

    async checkMachine(target) {
      const hubTransport = hub('checkMachine');
      // Not an error: another machine is checked over SSH as it is connected.
      if (target.kind !== 'local') return { machine: target, rows: [], deferred: CHECKED_WHEN_CONNECTED };
      const machine = await hubMachine(hubTransport);
      const rows = toCheckRows(await answer(hubTransport, 'GET', `/v1/machines/${encodeURIComponent(machine)}/check`));
      return { machine: target, rows };
    },

    async fixMachineRow(target, row) {
      const hubTransport = hub('fixMachineRow');
      onlyHubMachine(target);
      const before = await recheck(hubTransport, row);
      if (before === undefined) throw new Error('This machine has no such row any more; check it again.');
      if (before.status === 'ok') return before;
      const page = before.fix === 'install-page' ? installPage(row) : undefined;
      if (page === undefined) throw new Error(`PitCrew cannot fix “${before.label}” for you.`);
      // Nothing is installed: the person installs it from its page, then checks again.
      openPage?.(page);
      return before;
    },

    launcherOptions: () => Promise.reject(new Error('launcherOptions is not available yet.')),
    streamInstallHelper: () => unavailable('streamInstallHelper'),

    async agentAccounts() {
      const hubTransport = hub('agentAccounts');
      const machine = await hubMachine(hubTransport);
      return toAccounts(await answer(hubTransport, 'GET', `/v1/machines/${encodeURIComponent(machine)}/agents`));
    },

    async startSignIn(engine, target, method?: SignInMethod) {
      const hubTransport = hub('startSignIn');
      onlyHubMachine(target);
      const machine = await hubMachine(hubTransport);
      const body = method === 'device-code' ? JSON.stringify({ method: 'device_code' }) : '{}';
      return toStartSignInResult(toSignIn(await answer(hubTransport, 'POST', signInPath(machine, engine), body)));
    },

    async signInRunning(engine, target) {
      const hubTransport = hub('signInRunning');
      onlyHubMachine(target);
      const machine = await hubMachine(hubTransport);
      const res = await hubTransport.request('GET', signInPath(machine, engine));
      if (res.status === 404) return false;
      if (res.status !== 200) throw new Error(refusal(res));
      let body: unknown;
      try {
        body = JSON.parse(res.body);
      } catch {
        body = undefined;
      }
      return toSignIn(body).running;
    },

    async stopSignIn(engine, target) {
      const hubTransport = hub('stopSignIn');
      onlyHubMachine(target);
      const machine = await hubMachine(hubTransport);
      const res = await hubTransport.request('DELETE', signInPath(machine, engine));
      // `404`: there is none (any more) to stop.
      if (res.status !== 204 && res.status !== 404) throw new Error(refusal(res));
    },

    integrationStatus: () => Promise.reject(new Error('integrationStatus is not available yet.')),

    streamScan(target: ScanTarget, onEvent: (event: ScanProgressEvent) => void): Streamed {
      if (data === undefined) return unavailable('streamScan');
      const controller = new AbortController();
      let live = true;
      const emit = (event: ScanProgressEvent): void => {
        if (live) onEvent(event);
      };
      void (async () => {
        try {
          if (target.machine.kind !== 'local') {
            throw new Error('Only the hub’s own machine can be scanned for now.');
          }
          const machine = await ownMachine(data, controller.signal);
          // Cancelled before the scan went out (StrictMode's first mount): nothing started.
          if (!live) return;
          const res = await data.transport.request(
            'POST',
            `/v1/machines/${encodeURIComponent(machine)}/scan`,
            undefined,
            controller.signal,
          );
          if (res.status !== 200) {
            emit({ type: 'error', message: refusal(res) });
            return;
          }
          for (const frame of parseScanFrames(res.body)) {
            if (frame.type === 'progress') {
              emit({
                type: 'progress',
                scanned: frame.scanned,
                ...(frame.total === undefined ? {} : { total: frame.total }),
                ...(frame.path === undefined ? {} : { path: frame.path }),
              });
            } else if (frame.type === 'done') {
              if (live) lastScan = { machine, report: frame.report };
              emit({ type: 'done', result: toScanResult(frame.report) });
              return;
            } else {
              emit({ type: 'error', message: frame.message });
              return;
            }
          }
          emit({ type: 'error', message: 'The scan ended without a result.' });
        } catch (error) {
          emit({ type: 'error', message: messageOf(error) });
        }
      })();
      return {
        cancel() {
          live = false;
          controller.abort();
        },
      };
    },

    async createFromScan(selection: ProjectSelection[]): Promise<CreateFromScanResult> {
      if (data === undefined) return unavailable('createFromScan');
      const scan = lastScan;
      if (scan === undefined) throw new Error('Scan this machine first.');
      const taken = new Set((await data.projects()).map((p) => p.key));
      const result: CreateFromScanResult = { projects: [], workstreams: [] };
      for (const choice of selection) {
        const suggestion = scan.report.suggestions.find((s) => s.id === choice.suggestionId);
        if (suggestion === undefined) throw new Error(`“${choice.name}” is not in the last scan; scan again.`);
        let project = createdProjects.get(suggestion.id);
        if (project === undefined) {
          const root: Location = { machine: scan.machine, path: suggestion.path };
          project = await createProject(data, nameOr(choice.name, suggestion.name), root, taken);
          createdProjects.set(suggestion.id, project);
        }
        result.projects.push(project);
        // The project's default workstream (at its root) always comes with it, so every session
        // under the project is linked (api-v1.md, "Machine scan"), even if the selection left it out.
        const main = suggestion.workstreams.find((w) => w.kind === 'main');
        const picks =
          main === undefined || choice.workstreams.some((w) => w.suggestionId === main.id)
            ? choice.workstreams
            : [{ suggestionId: main.id, name: main.name }, ...choice.workstreams];
        for (const pick of picks) {
          const found = findWorkstream(scan.report, pick.suggestionId);
          if (found === undefined) throw new Error(`“${pick.name}” is not in the last scan; scan again.`);
          const memo = `${project.id}\n${pick.suggestionId}`;
          let workstream = createdWorkstreams.get(memo);
          if (workstream === undefined) {
            workstream = await data.createWorkstream({
              project: project.id,
              name: nameOr(pick.name, found.workstream.name),
              locations: [locationOf(scan.machine, found)],
            });
            createdWorkstreams.set(memo, workstream);
          }
          result.workstreams.push(workstream);
        }
      }
      return result;
    },

    async importSessions(filter) {
      if (transport === undefined) return unavailable('importSessions');
      const res = await transport.request('POST', '/v1/import/dry-run', JSON.stringify(filter));
      if (res.status !== 200) throw new Error(refusal(res));
      return JSON.parse(res.body) as { count: number; subagents?: number };
    },
    async commitImport(filter) {
      if (transport === undefined) return unavailable('commitImport');
      const res = await transport.request('PUT', '/v1/import', JSON.stringify(filter));
      if (res.status !== 200) throw new Error(refusal(res));
      return JSON.parse(res.body) as { imported: number; subagents?: number };
    },
    async hooksDiff() {
      if (hooksData === undefined || transport === undefined) return unavailable('hooksDiff');
      const machine = await ownMachine(hooksData, new AbortController().signal);
      const res = await transport.request('POST', `/v1/machines/${encodeURIComponent(machine)}/hooks/diff`);
      if (res.status !== 200) throw new Error(refusal(res));
      return JSON.parse(res.body) as import('./api.ts').HooksDiff;
    },
    async installHooks(preview) {
      if (hooksData === undefined || transport === undefined) return unavailable('installHooks');
      const machine = await ownMachine(hooksData, new AbortController().signal);
      const res = await transport.request('POST', `/v1/machines/${encodeURIComponent(machine)}/hooks/install`, JSON.stringify({ revision: preview.revision }));
      if (res.status !== 200) throw new Error(refusal(res));
    },
    async readSafety() {
      if (transport === undefined) return unavailable('readSafety');
      const res = await transport.request('GET', '/v1/safety');
      if (res.status !== 200) throw new Error(refusal(res));
      const wire = JSON.parse(res.body) as { permission_mode: string; back_office_enabled: boolean; back_office_caps: { max_auto_accept_per_hour: number } };
      return { permissionMode: wire.permission_mode.replaceAll('_', '-') as import('./api.ts').PermissionMode, backOfficeEnabled: wire.back_office_enabled, backOfficeCaps: { maxAutoAcceptPerHour: wire.back_office_caps.max_auto_accept_per_hour } };
    },
    async saveSafety(settings) {
      if (transport === undefined) return unavailable('saveSafety');
      const res = await transport.request('PUT', '/v1/safety', JSON.stringify({ permission_mode: settings.permissionMode.replaceAll('-', '_'), back_office_enabled: settings.backOfficeEnabled, back_office_caps: { max_auto_accept_per_hour: settings.backOfficeCaps.maxAutoAcceptPerHour } }));
      if (res.status !== 200) throw new Error(refusal(res));
    },
  };
}
