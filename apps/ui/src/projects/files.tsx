// The workstream page's Files tab: a location picker, the folder tree and the shared file viewer
// (`file-viewer.tsx`, which the console's workbench uses too).

import { useEffect, useMemo, useState } from 'react';
import { useApi, type Workstream } from '../data/index.ts';
import { fileClient } from '../data/files.ts';
import { FileTree, FileViewer } from './file-viewer.tsx';

export { fileProblem, viewerKind } from './file-viewer.tsx';

export function FilesTab({ workstream, onDirty, onBusy }: { workstream: Workstream; onDirty: (dirty: boolean) => void; onBusy: (busy: boolean) => void }) {
  const api = useApi();
  const [location, setLocation] = useState(0);
  const [path, setPath] = useState<string>();
  const [dirty, setDirty] = useState(false);
  const [busy, setBusy] = useState(false);
  const client = useMemo(() => fileClient(api, workstream.id, location), [api, workstream.id, location]);
  useEffect(() => { onDirty(dirty); }, [dirty, onDirty]);
  useEffect(() => { onBusy(busy); }, [busy, onBusy]);
  useEffect(() => {
    if (!dirty && !busy) return;
    const warn = (event: BeforeUnloadEvent) => { event.preventDefault(); event.returnValue = ''; };
    window.addEventListener('beforeunload', warn);
    return () => window.removeEventListener('beforeunload', warn);
  }, [dirty, busy]);
  const canLeave = () => !busy && (!dirty || window.confirm('Discard unsaved changes?'));
  if (workstream.locations.length === 0) return <p>No folders in this workstream.</p>;
  return <section aria-label="Workstream files" className="space-y-3">
    {workstream.locations.length > 1 ? <label className="block text-sm">Location
      <select className="ml-2 max-w-full rounded-sm border border-line bg-card p-1" value={location} disabled={busy} onChange={(event) => {
        if (canLeave()) { setLocation(Number(event.target.value)); setPath(undefined); setDirty(false); }
      }}>{workstream.locations.map((loc, index) => <option key={index} value={index}>{loc.path}</option>)}</select>
    </label> : <p className="break-all text-sm text-ink-2">{workstream.locations[0]?.path}</p>}
    <div className="flex flex-col gap-4 md:flex-row">
      <FileTree key={location} className="md:w-64 md:shrink-0" client={client} scope={['files', workstream.id, location]} openFile={(next) => {
        if (next !== path && canLeave()) { setPath(next); setDirty(false); }
      }} />
      {path === undefined ? <p className="text-sm text-ink-2">Choose a file to view.</p> : <FileViewer key={`${location}:${path}`} client={client} path={path} onDirty={setDirty} onBusy={setBusy} />}
    </div>
  </section>;
}
