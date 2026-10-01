// `/connect` (`paths.connect()`): connect a remote machine, in the desktop app. A root route, so it
// also runs when there is no workspace yet. In a browser there is no gateway to do it with, and
// the page says so.

import { Link, useRouter } from '@tanstack/react-router';
import { useRemoteGateway } from '../../data/index.ts';
import { paths } from '../../shell/index.ts';
import { ConnectWizard } from './connect-wizard.tsx';

export function ConnectPage() {
  const router = useRouter();
  const remote = useRemoteGateway();
  if (remote === null) {
    return (
      <main className="grid min-h-dvh place-items-center bg-bg px-6 text-ink">
        <div className="flex max-w-md flex-col items-start gap-3">
          <h1 className="text-lg font-semibold">Connect a remote machine</h1>
          <p className="text-sm text-ink-2">
            Connecting a remote machine needs the PitCrew desktop app: it reaches the machine over your own SSH
            connection. This browser cannot.
          </p>
          <Link to="/" className="text-sm font-medium text-accent-text underline underline-offset-2">
            Back to PitCrew
          </Link>
        </div>
      </main>
    );
  }
  return (
    <ConnectWizard
      remote={remote}
      onOpen={(workspace) => void router.navigate({ href: paths.workspace(workspace.id) })}
      onCancel={() => (router.history.canGoBack() ? router.history.back() : void router.navigate({ href: '/' }))}
    />
  );
}
