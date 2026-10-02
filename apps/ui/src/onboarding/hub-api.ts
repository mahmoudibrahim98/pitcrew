// The real `OnboardingApi`: what the hub and the desktop gateway can do today.
// - `setupWorkspace` is `POST /v1/setup`, through the data layer's `setUp` for the workspace in
//   view (so its cache knows at once that it is set up);
// - `discoverHosts` is the gateway's `sshHosts`, in the desktop app;
// - everything else has no backend yet: it is listed in `unavailable`, rejects if called, and
//   `stepsFor` leaves its step out. So the real first run is Welcome, Workspace, Done.

import { ApiError, SetupConflict, type RemoteGateway, type Setup, type SetupResult } from '../data/index.ts';
import { SetupRefused, type OnboardingApi, type OnboardingCall } from './api.ts';
import { fieldOfMessage } from './validation.ts';

const NOT_YET: readonly OnboardingCall[] = [
  'checkMachine',
  'fixMachineRow',
  'launcherOptions',
  'streamInstallHelper',
  'agentAccounts',
  'startSignIn',
  'integrationStatus',
  'streamScan',
  'createFromScan',
  'importSessions',
  'commitImport',
  'hooksDiff',
  'installHooks',
  'saveSafety',
];

export interface HubOnboardingOptions {
  /** `POST /v1/setup` for the workspace in view (`useSetUp()`); none outside a workspace. */
  setUp?: ((setup: Setup) => Promise<SetupResult>) | undefined;
  /** The gateway's remote commands; `null` in a browser. */
  remote?: RemoteGateway | null | undefined;
}

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

export function createHubOnboardingApi(options: HubOnboardingOptions = {}): OnboardingApi {
  const { setUp } = options;
  const remote = options.remote ?? null;
  const missing = new Set<OnboardingCall>(NOT_YET);
  if (setUp === undefined) missing.add('setupWorkspace');
  if (remote === null) missing.add('discoverHosts');

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
    streamScan: () => {
      throw new Error('streamScan is not available yet.');
    },
    createFromScan: () => Promise.reject(new Error('createFromScan is not available yet.')),
    importSessions: () => Promise.reject(new Error('importSessions is not available yet.')),
    commitImport: () => Promise.reject(new Error('commitImport is not available yet.')),
    hooksDiff: () => Promise.reject(new Error('hooksDiff is not available yet.')),
    installHooks: () => Promise.reject(new Error('installHooks is not available yet.')),
    saveSafety: () => Promise.reject(new Error('saveSafety is not available yet.')),
  };
}
