import { Link } from '@tanstack/react-router';
import { useWorkspaceId } from '../layout.ts';
import { paths } from '../paths.ts';

function Message({ home }: { home: string }) {
  return (
    <div className="mx-auto flex max-w-md flex-col items-start gap-2 px-6 py-16">
      <p className="font-mono text-xs text-ink-2">404</p>
      <h1 className="text-xl font-semibold">Page not found</h1>
      <p className="text-sm text-ink-2">Nothing lives at this address. It may have moved, or the link is wrong.</p>
      <Link to={home} className="text-sm font-medium text-accent-text underline-offset-2 hover:underline">
        Go home
      </Link>
    </div>
  );
}

/** Inside a workspace: rendered in the frame's main area. */
export function NotFoundPage() {
  const ws = useWorkspaceId();
  return <Message home={ws === '' ? '/' : paths.workspace(ws)} />;
}

/** Outside any workspace. */
export function RootNotFound() {
  return (
    <main className="min-h-dvh bg-bg text-ink">
      <Message home="/" />
    </main>
  );
}
