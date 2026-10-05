import { useEffect, useMemo, useState } from 'react';
import { fileClient, type FileClient } from '../data/files.ts';
import { useApi, type Workstream } from '../data/index.ts';
import { Dialog, DialogContent } from '../design/index.ts';
import { hasMod } from '../lib/platform.ts';
import { FileTree, fileProblem } from './file-viewer.tsx';
import { collectFiles, fileScore } from './file-search.ts';
import type { TabRef } from '../console/workbench/layout.ts';

export function FileExplorer({ workstream, openFile, previewFile }: { workstream: Workstream; openFile(ref: TabRef): void; previewFile?: ((ref: TabRef) => void) | undefined }) {
  const api = useApi();
  const [location, setLocation] = useState(0);
  const [rootPath, setRootPath] = useState('');
  const [quick, setQuick] = useState(false);
  const [hidden, setHidden] = useState(false);
  const client = useMemo(() => fileClient(api, workstream.id, location), [api, workstream.id, location]);
  useEffect(() => {
    const key = (event: KeyboardEvent) => {
      if (event.defaultPrevented || !hasMod(event) || event.altKey || event.shiftKey || event.key.toLowerCase() !== 'p') return;
      if (event.target instanceof Element && event.target.closest('[data-shell-keys="none"]')) return;
      event.preventDefault();
      if (!event.repeat) setQuick(true);
    };
    const intent = () => setQuick(true);
    const folder = (event: Event) => {
      const detail = (event as CustomEvent<{ workstream: string; location: number; directory: string }>).detail;
      if (detail.workstream === workstream.id) { setLocation(detail.location); setRootPath(detail.directory); }
    };
    window.addEventListener('pitcrew:file-folder', folder);
    window.addEventListener('keydown', key);
    window.addEventListener('pitcrew:quick-open', intent);
    return () => { window.removeEventListener('pitcrew:file-folder', folder); window.removeEventListener('keydown', key); window.removeEventListener('pitcrew:quick-open', intent); };
  }, [workstream.id]);
  const searchClient = useMemo(() => ({ list: async (path: string, signal?: AbortSignal) => {
    if (path === '') return { entries: workstream.locations.map((_, index) => ({ name: String(index), kind: 'folder' as const, size: 0, modified_at: null })), truncated: false };
    const slash = path.indexOf('/');
    const index = Number(slash === -1 ? path : path.slice(0, slash));
    return fileClient(api, workstream.id, index).list(slash === -1 ? '' : path.slice(slash + 1), signal);
  } }), [api, workstream]);
  const open = (path: string) => { (previewFile ?? openFile)({ kind: 'file', workstream: workstream.id, location, path }); setQuick(false); };
  return <aside aria-label="File explorer" className="min-w-0 space-y-2 rounded-sm border border-line bg-card p-3">
    <details open>
      <summary className="cursor-pointer text-sm font-medium">Explorer · {workstream.name}</summary>
      <div className="space-y-2 pt-2">
        {workstream.locations.length === 0 ? <p>No folders in this workstream.</p> : <>
          <label className="block text-xs">Location <select value={location} onChange={e => { setLocation(Number(e.target.value)); setRootPath(''); }} className="max-w-full border border-line bg-card">
            {workstream.locations.map((loc, index) => <option key={index} value={index}>{loc.path}</option>)}
          </select></label>
          <button className="rounded-sm border border-line px-2 py-1 text-sm" onClick={() => setQuick(true)}>Quick open… <kbd>Ctrl P</kbd></button>
          <label className="block text-xs"><input type="checkbox" checked={hidden} onChange={e => setHidden(e.target.checked)} /> Show hidden</label>
          {rootPath && <button className="text-xs text-accent-text underline" onClick={() => setRootPath('')}>Root / {rootPath}</button>}
          <FileTree label="Explorer folder tree" key={`${location}:${rootPath}`} rootPath={rootPath} client={client} scope={['files', workstream.id, location]} openFile={open} showHidden={hidden} />
        </>}
      </div>
    </details>
    <Dialog open={quick} onOpenChange={setQuick}>
      <DialogContent title="Quick open" description="Search filenames across this workstream’s locations. Results are bounded; links are never followed.">
        {quick && <QuickFiles client={searchClient} hidden={hidden} locations={workstream.locations.map(loc => loc.path)} open={target => { const slash = target.indexOf('/'); openFile({ kind: 'file', workstream: workstream.id, location: Number(target.slice(0, slash)), path: target.slice(slash + 1) }); setQuick(false); }} />}
      </DialogContent>
    </Dialog>
  </aside>;
}

function QuickFiles({ client, hidden, locations, open }: { client: Pick<FileClient, 'list'>; hidden: boolean; locations: string[]; open(path: string): void }) {
  const [query, setQuery] = useState('');
  const [result, setResult] = useState<Awaited<ReturnType<typeof collectFiles>>>();
  const [error, setError] = useState<unknown>();
  useEffect(() => {
    let active = true;
    const controller = new AbortController();
    const timeout = window.setTimeout(() => controller.abort(), 15_000);
    void collectFiles(client, hidden, controller.signal).then(value => { if (active) setResult(value); }).catch(e => { if (active) setError(e); });
    return () => { active = false; controller.abort(); window.clearTimeout(timeout); };
  }, [client, hidden]);
  const matches = (result?.paths ?? []).map(path => ({ path, score: fileScore(path, query) }))
    .filter((match): match is { path: string; score: number } => match.score !== undefined)
    .sort((a, b) => b.score - a.score || a.path.localeCompare(b.path)).slice(0, 100);
  return <div className="space-y-2">
    <label className="block">Filename <input autoFocus value={query} onChange={e => setQuery(e.target.value)} className="w-full rounded-sm border border-line bg-card p-2" onKeyDown={e => {
      if (e.key === 'Enter' && matches[0]) { e.preventDefault(); open(matches[0].path); }
      if (e.key === 'ArrowDown') { e.preventDefault(); document.querySelector<HTMLButtonElement>('[data-quick-result]')?.focus(); }
    }} /></label>
    {!result && !error && <p role="status">Finding files…</p>}
    {error != null && <p role="alert">{fileProblem(error)}</p>}
    {result?.truncated && <p role="status">Search truncated; some files are omitted.</p>}
    {result?.unavailable && <p role="status">Some folders could not be listed.</p>}
    {result && matches.length === 0 && <p>No matching files.</p>}
    <ul className="max-h-80 overflow-auto" onKeyDown={e => {
      if (!['ArrowDown', 'ArrowUp'].includes(e.key)) return;
      const buttons = [...e.currentTarget.querySelectorAll<HTMLButtonElement>('button')];
      const at = buttons.indexOf(e.target as HTMLButtonElement);
      e.preventDefault(); buttons[Math.max(0, Math.min(buttons.length - 1, at + (e.key === 'ArrowDown' ? 1 : -1)))]?.focus();
    }}>{matches.map(({ path }) => <li key={path}><button data-quick-result="" className="w-full rounded-sm px-2 py-1 text-left font-mono text-sm hover:bg-hover" onClick={() => open(path)}>{path.slice(path.indexOf('/') + 1)}{locations.length > 1 && ` · ${locations[Number(path.split('/')[0])]}`}</button></li>)}</ul>
    {matches.length === 100 && <p className="text-xs">Showing the first 100 matches. Refine the filename to see more.</p>}
  </div>;
}
