import { useSession } from '../data/index.ts';
import { LinkSessionDialog } from '../console/link-session.tsx';
import { useShell } from './store.ts';

export function PaletteLinkSession({ id }: { id: string }) {
  const session = useSession(id).data;
  return session === undefined ? null : <LinkSessionDialog session={session} onClose={() => useShell.getState().setLinkingSession(null)} />;
}
