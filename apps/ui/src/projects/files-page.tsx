import { useRouter } from '@tanstack/react-router';
import { useEffect, useState } from 'react';
import { useWorkstreams } from '../data/index.ts';
import { openTab, type TabRef } from '../console/workbench/layout.ts';
import { workbenchStore } from '../console/workbench/store.ts';
import { paths, useWorkspaceId } from '../shell/index.ts';
import { FileExplorer } from './file-explorer.tsx';

export function FilesPage() {
  const router = useRouter();
  const ws = useWorkspaceId();
  const streams = useWorkstreams();
  const [selected, setSelected] = useState<string>();
  const stream = streams.data?.find(s => s.id === selected) ?? streams.data?.[0];
  useEffect(() => {
    if (stream && new URLSearchParams(router.state.location.searchStr).get('quick') === '1') window.dispatchEvent(new Event('pitcrew:quick-open'));
  }, [stream, router]);
  const open = (ref: TabRef) => {
    workbenchStore(ws).update(layout => openTab(layout, ref));
    void router.navigate({ href: paths.console(ws) });
  };
  return <div className="space-y-4 p-6">
    <h1 className="text-2xl font-semibold">Files</h1>
    {streams.error && <p role="alert">Could not load workstreams.</p>}
    {streams.isPending && <p role="status">Loading workstreams…</p>}
    {streams.data?.length === 0 && <p>No workstreams yet.</p>}
    {stream && <>
      <label>Workstream <select className="border border-line bg-card p-1" value={stream.id} onChange={event => setSelected(event.target.value)}>
        {streams.data?.map(s => <option key={s.id} value={s.id}>{s.name}</option>)}
      </select></label>
      <FileExplorer key={stream.id} workstream={stream} openFile={open} />
    </>}
  </div>;
}
