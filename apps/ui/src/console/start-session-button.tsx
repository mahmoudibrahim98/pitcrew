import { lazy, Suspense, useState } from 'react';
import type { Location, WorkstreamId } from '../data/index.ts';
import { Button, Dialog, DialogContent } from '../design/index.ts';

const NewSession = lazy(() => import('./new-session.tsx'));

export function StartSessionButton({ locations, workstream }: { locations?: readonly Location[]; workstream?: WorkstreamId }) {
  const [open, setOpen] = useState(false);
  return <>
    <Button variant="ghost" onClick={() => setOpen(true)}>Start session</Button>
    <Dialog open={open} onOpenChange={setOpen}>
      <DialogContent title="New session">
        {open && <Suspense fallback={<p role="status">Loading…</p>}><NewSession {...(workstream === undefined ? {} : { workstream })} close={() => setOpen(false)} {...(locations === undefined ? {} : { locations })} /></Suspense>}
      </DialogContent>
    </Dialog>
  </>;
}
