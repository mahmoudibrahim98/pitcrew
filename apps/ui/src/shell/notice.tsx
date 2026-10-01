// A brief, dismissible message at the top of the window: currently only for a deep link whose
// workspace this app no longer lists (`gateway-navigate.ts`). Lives at the root, above the
// router's `Outlet`, so it survives `/`'s redirect into a workspace.

import { Button } from '../design/index.ts';
import { useShell } from './store.ts';

export function Notice() {
  const notice = useShell((s) => s.notice);
  const setNotice = useShell((s) => s.setNotice);
  if (notice === null) return null;
  return (
    <div
      role="status"
      className="fixed inset-x-0 top-0 z-[60] flex items-center justify-center gap-3 border-b border-line bg-card px-4 py-2 text-sm text-ink shadow-pop"
    >
      <p>{notice}</p>
      <Button onClick={() => setNotice(null)}>Dismiss</Button>
    </div>
  );
}
