// The first-run wizard at `paths.setup(ws)` (`/w/$ws/onboarding`), where the shell sends a
// workspace that needs setup. A lazy route component (`routes.tsx`), so it is its own chunk.
//
// It runs against the real hub (`hub-api.ts`): Welcome, Workspace, Scan, Create, Import, Done (a remote
// hub's, without Scan and Create). A workspace that is set up already goes Home instead, unless this visit is the one that set it up (its Done step is
// still to come). A development build can run every step against the fake instead with
// `?onboarding=fake`; a production build cannot (the branch, and the fake with it, is dropped at
// build time).

import { useQueryClient } from '@tanstack/react-query';
import { useRouter, useSearch } from '@tanstack/react-router';
import { lazy, Suspense, useEffect, useMemo, useState } from 'react';
import { keys, useApi, useGatewayWorkspace, useRemoteGateway, useSetUp, useWorkspace } from '../data/index.ts';
import { paths, useWorkspaceId } from '../shell/index.ts';
import { OnboardingApiProvider } from './api-context.tsx';
import type { OnboardingApi } from './api.ts';
import { createHubOnboardingApi } from './hub-api.ts';
import { WizardProvider, type WizardDefaults } from './wizard-context.tsx';
import { WizardShell } from './wizard-shell.tsx';

const FakeFirstRun = import.meta.env.DEV
  ? lazy(() => import('./fake-first-run.tsx').then((m) => ({ default: m.FakeFirstRun })))
  : null;

export function FirstRunPage() {
  const search: { onboarding?: unknown } = useSearch({ strict: false });
  if (FakeFirstRun !== null && search.onboarding === 'fake') {
    return (
      <Suspense fallback={null}>
        <FakeFirstRun />
      </Suspense>
    );
  }
  return <HubFirstRun />;
}

/** Replaces the location with Home. */
function GoHome() {
  const router = useRouter();
  const ws = useWorkspaceId();
  useEffect(() => {
    void router.navigate({ href: paths.home(ws), replace: true });
  }, [router, ws]);
  return null;
}

function HubFirstRun() {
  const queries = useQueryClient();
  const setUp = useSetUp();
  const remote = useRemoteGateway();
  const data = useApi();
  const gateway = useGatewayWorkspace();
  const info = useWorkspace().data;
  // Set as the setup is sent, before the hub answers: the cache then says "set up", and this
  // visit's own Done step must still show rather than be sent Home.
  const [sent, setSent] = useState(false);
  // A remote hub's own machine is that machine, not this computer: start from the gateway's name.
  const remoteName = gateway?.kind === 'remote' ? gateway.name : undefined;
  // The scan reads the hub's own machine's agent homes, which on a remote hub are the remote
  // machine's: scanning one is a later step, so a remote hub's first run stays without Scan and
  // Create.
  const scanData = remoteName === undefined ? data : undefined;
  const api = useMemo((): OnboardingApi => {
    const hub = createHubOnboardingApi({ setUp, remote, data: scanData, hooksData: scanData, transport: data.transport });
    return {
      ...hub,
      commitImport: async (filter) => {
        const result = await hub.commitImport(filter);
        await Promise.all([['sessions'], keys.events, keys.recaps.all].map((queryKey) =>
          queries.resetQueries({ queryKey })));
        return result;
      },
      setupWorkspace: (input) => {
        setSent(true);
        return hub.setupWorkspace(input);
      },
    };
  }, [setUp, remote, scanData, data, queries]);
  const defaults = useMemo(
    (): WizardDefaults =>
      remoteName === undefined ? {} : { machineName: remoteName, machineLabel: 'The remote machine’s name' },
    [remoteName],
  );

  if (info === undefined) {
    return (
      <p role="status" className="px-8 py-10 text-sm text-ink-2">
        Loading…
      </p>
    );
  }
  if (info.setup_needed !== true && !sent) return <GoHome />;
  return (
    <OnboardingApiProvider api={api}>
      <WizardProvider defaults={defaults}>
        <WizardShell />
      </WizardProvider>
    </OnboardingApiProvider>
  );
}
