// The real `OnboardingApi`: what the hub and the desktop gateway can do today.
// - `setupWorkspace` is `POST /v1/setup`, through the data layer's `setUp` for the workspace in
//   view (so its cache knows at once that it is set up);
// - `discoverHosts` is the gateway's `sshHosts`, in the desktop app;
// - `streamScan` is `POST /v1/machines/{id}/scan` on the hub's own machine, and `createFromScan`
//   is `POST /v1/projects` and `POST /v1/workstreams` from that scan's suggestions, both through
//   the data layer's client for the workspace in view;
// - everything else has no backend yet: it is listed in `unavailable`, rejects if called, and
//   `stepsFor` leaves its step out. So the real first run is Welcome, Workspace, Scan, Create, Import, Done.
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
  type CreateFromScanResult,
  type OnboardingApi,
  type OnboardingCall,
  type ProjectSelection,
  type ScanProgressEvent,
  type ScanTarget,
  type Streamed,
} from './api.ts';
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
  'checkMachine',
  'fixMachineRow',
  'launcherOptions',
  'streamInstallHelper',
  'agentAccounts',
  'startSignIn',
  'integrationStatus',
  'hooksDiff',
  'installHooks',
  'saveSafety',
];

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
  const remote = options.remote ?? null;
  const transport = options.transport ?? data?.transport;
  const missing = new Set<OnboardingCall>(NOT_YET);
  if (setUp === undefined) missing.add('setupWorkspace');
  if (remote === null) missing.add('discoverHosts');
  if (transport === undefined) {
    missing.add('importSessions');
    missing.add('commitImport');
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

    checkMachine: () => Promise.reject(new Error('checkMachine is not available yet.')),
    fixMachineRow: () => Promise.reject(new Error('fixMachineRow is not available yet.')),
    launcherOptions: () => Promise.reject(new Error('launcherOptions is not available yet.')),
    streamInstallHelper: (_options, onEvent) => {
      onEvent({ type: 'error', message: 'Installing the helper is not available yet.' });
      return { cancel() {} };
    },
    agentAccounts: () => Promise.reject(new Error('agentAccounts is not available yet.')),
    startSignIn: () => Promise.reject(new Error('startSignIn is not available yet.')),
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
        for (const pick of choice.workstreams) {
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
      return JSON.parse(res.body) as { count: number };
    },
    async commitImport(filter) {
      if (transport === undefined) return unavailable('commitImport');
      const res = await transport.request('PUT', '/v1/import', JSON.stringify(filter));
      if (res.status !== 200) throw new Error(refusal(res));
      return JSON.parse(res.body) as { imported: number };
    },
    hooksDiff: () => Promise.reject(new Error('hooksDiff is not available yet.')),
    installHooks: () => Promise.reject(new Error('installHooks is not available yet.')),
    saveSafety: () => Promise.reject(new Error('saveSafety is not available yet.')),
  };
}
