import { useEffect, useMemo, useState, type KeyboardEvent } from 'react';
import { ApiError, useApi, useLiveQuery, type Workstream } from '../data/index.ts';
import { fileClient, type FileContent } from '../data/files.ts';

type Client = ReturnType<typeof fileClient>;
const BUTTON = 'rounded-sm border border-line px-3 py-1 text-sm disabled:opacity-50';

export function fileProblem(error: unknown): string {
  if (error instanceof ApiError) {
    if (error.status === 403) return 'Not allowed';
    if (error.status === 404) return 'Gone';
    if (error.status === 501) return "Files on another machine aren't supported yet.";
    if (error.status === 413) return `Too large to show (${error.size ?? 'unknown'} bytes)`;
  }
  return error instanceof Error ? error.message : 'Could not load files';
}

export function viewerKind(file: FileContent): 'image' | 'text' | 'binary' {
  if (file.media_type === 'image/png' || file.media_type === 'image/jpeg') return 'image';
  return file.encoding === 'utf8' ? 'text' : 'binary';
}

function imageSource(file: FileContent) {
  const base64 = file.encoding === 'base64' ? file.content : btoa(Array.from(new TextEncoder().encode(file.content), (byte) => String.fromCharCode(byte)).join(''));
  return `data:${file.media_type};base64,${base64}`;
}

function Folder({ client, scope, path, openFile }: { client: Client; scope: readonly unknown[]; path: string; openFile: (path: string) => void }) {
  const listing = useLiveQuery({ queryKey: [...scope, 'list', path], queryFn: ({ signal }) => client.list(path, signal), retry: false, staleTime: 0 });
  const [expanded, setExpanded] = useState<string[]>([]);
  if (listing.isPending) return <p role="status">Loading folder…</p>;
  if (listing.error) return <div><p role="alert">{fileProblem(listing.error)}</p><button className={BUTTON} onClick={() => void listing.refetch()}>Retry folder</button></div>;
  return <>
    {listing.data.entries.length === 0 && <p className="text-sm text-ink-2">Empty folder</p>}
    <ul className="space-y-1 pl-3">
      {listing.data.entries.map((entry) => {
        const child = path ? `${path}/${entry.name}` : entry.name;
        const isOpen = expanded.includes(child);
        return <li key={entry.name}>
          {entry.kind === 'link' ? <span className="text-sm text-ink-2">{entry.name} (link, cannot open)</span> :
            <button className="rounded-sm px-2 py-1 text-left text-sm hover:bg-sunken" aria-expanded={entry.kind === 'folder' ? isOpen : undefined}
              onClick={() => entry.kind === 'folder' ? setExpanded(isOpen ? expanded.filter((p) => p !== child) : [...expanded, child]) : openFile(child)}
              onKeyDown={(event) => {
                if (entry.kind === 'folder' && (event.key === 'ArrowRight' || event.key === 'ArrowLeft')) {
                  event.preventDefault();
                  setExpanded(event.key === 'ArrowRight' ? [...new Set([...expanded, child])] : expanded.filter((p) => p !== child));
                }
              }}>
              {entry.kind === 'folder' ? `${isOpen ? '▾' : '▸'} ${entry.name}` : entry.name}
            </button>}
          {entry.kind === 'folder' && isOpen && <Folder client={client} scope={scope} path={child} openFile={openFile} />}
        </li>;
      })}
    </ul>
    {listing.data.truncated && <p role="status" className="text-sm text-ink-2">Listing truncated; some entries are omitted.</p>}
  </>;
}

function treeKeys(event: KeyboardEvent<HTMLElement>) {
  if (!['ArrowDown', 'ArrowUp', 'Home', 'End'].includes(event.key)) return;
  const buttons = [...event.currentTarget.querySelectorAll<HTMLButtonElement>('button')];
  const index = buttons.indexOf(event.target as HTMLButtonElement);
  if (index < 0) return;
  event.preventDefault();
  const next = event.key === 'Home' ? 0 : event.key === 'End' ? buttons.length - 1 : Math.max(0, Math.min(buttons.length - 1, index + (event.key === 'ArrowDown' ? 1 : -1)));
  buttons[next]?.focus();
}

function FileView({ client, path, onDirty, onBusy }: { client: Client; path: string; onDirty: (dirty: boolean) => void; onBusy: (busy: boolean) => void }) {
  const [file, setFile] = useState<FileContent>();
  const [draft, setDraft] = useState<string>();
  const [error, setError] = useState<unknown>();
  const [busy, setBusy] = useState(false);
  const [saved, setSaved] = useState(false);
  const dirty = draft !== undefined && draft !== file?.content;
  useEffect(() => { onDirty(dirty); }, [dirty, onDirty]);
  useEffect(() => { onBusy(busy); }, [busy, onBusy]);
  useEffect(() => {
    const controller = new AbortController();
    void client.read(path, controller.signal).then((value) => {
      if (!controller.signal.aborted) setFile(value);
    }).catch((reason: unknown) => { if (!controller.signal.aborted) setError(reason); });
    return () => controller.abort();
  }, [client, path]);

  async function save(overwrite = false) {
    if (!file || draft === undefined) return;
    setBusy(true); setError(undefined); setSaved(false);
    try {
      const revision = overwrite ? (await client.read(path)).revision : file.revision;
      const result = await client.write(path, revision, draft);
      setFile(result); setDraft(undefined); setSaved(true);
    } catch (reason) { setError(reason); }
    finally { setBusy(false); }
  }
  async function reload() {
    if (dirty && !window.confirm('Discard unsaved changes and reload?')) return;
    setBusy(true);
    try { setFile(await client.read(path)); setDraft(undefined); setError(undefined); setSaved(false); }
    catch (reason) { setError(reason); }
    finally { setBusy(false); }
  }
  const conflict = error instanceof ApiError && error.status === 409;
  return <section aria-label="File viewer" className="min-w-0 flex-1 space-y-3">
    <h2 className="break-all font-medium">{path}</h2>
    {error !== undefined && <div role="alert">
      <p>{conflict ? 'Changed since you opened it' : fileProblem(error)}</p>
      {conflict && <div className="flex gap-2"><button className={BUTTON} disabled={busy} onClick={() => void reload()}>Reload</button><button className={BUTTON} disabled={busy} onClick={() => void save(true)}>Overwrite</button></div>}
      {!conflict && file === undefined && <button className={BUTTON} disabled={busy} onClick={() => void reload()}>Retry file</button>}
    </div>}
    {file === undefined && error === undefined && <p role="status">Loading file…</p>}
    {saved && <p role="status">Saved</p>}
    {file !== undefined && <>
      {viewerKind(file) === 'image' && <img alt={path} className="max-w-full" src={imageSource(file)} />}
      {viewerKind(file) === 'binary' && <p>Binary, {file.size} bytes</p>}
      {viewerKind(file) === 'text' && <>
        {draft === undefined ? <>
          <button className={BUTTON} onClick={() => { setDraft(file.content); setSaved(false); }}>Edit</button>
          <div className="max-h-[65vh] overflow-auto rounded-sm border border-line bg-sunken p-3 font-mono text-sm" tabIndex={0} aria-label="File text">
            {file.content.split('\n').map((line, index) => <div key={index} className="flex"><span aria-hidden="true" className="mr-4 w-10 shrink-0 text-right text-ink-2">{index + 1}</span><pre className="m-0">{line || ' '}</pre></div>)}
          </div>
        </> : <>
          <textarea aria-label="Edit file text" className="min-h-80 w-full rounded-sm border border-line bg-card p-3 font-mono text-sm" value={draft} disabled={busy} onChange={(event) => setDraft(event.target.value)} />
          <div className="flex gap-2"><button className={BUTTON} disabled={busy} onClick={() => void save()}>Save</button><button className={BUTTON} disabled={busy} onClick={() => { if (!dirty || window.confirm('Discard unsaved changes?')) setDraft(undefined); }}>Cancel edit</button></div>
        </>}
      </>}
    </>}
  </section>;
}

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
    if (!dirty) return;
    const warn = (event: BeforeUnloadEvent) => { event.preventDefault(); event.returnValue = ''; };
    window.addEventListener('beforeunload', warn);
    return () => window.removeEventListener('beforeunload', warn);
  }, [dirty]);
  const canLeave = () => !busy && (!dirty || window.confirm('Discard unsaved changes?'));
  if (workstream.locations.length === 0) return <p>No folders in this workstream.</p>;
  return <section aria-label="Workstream files" className="space-y-3">
    {workstream.locations.length > 1 ? <label className="block text-sm">Location
      <select className="ml-2 max-w-full rounded-sm border border-line bg-card p-1" value={location} disabled={busy} onChange={(event) => {
        if (canLeave()) { setLocation(Number(event.target.value)); setPath(undefined); setDirty(false); }
      }}>{workstream.locations.map((loc, index) => <option key={index} value={index}>{loc.path}</option>)}</select>
    </label> : <p className="break-all text-sm text-ink-2">{workstream.locations[0]?.path}</p>}
    <div className="flex flex-col gap-4 md:flex-row">
      <nav aria-label="Folder tree" onKeyDown={treeKeys} className="min-w-0 md:w-64 md:shrink-0">
        <Folder key={location} client={client} scope={['files', workstream.id, location]} path="" openFile={(next) => {
          if (next !== path && canLeave()) { setPath(next); setDirty(false); }
        }} />
      </nav>
      {path === undefined ? <p className="text-sm text-ink-2">Choose a file to view.</p> : <FileView key={`${location}:${path}`} client={client} path={path} onDirty={setDirty} onBusy={setBusy} />}
    </div>
  </section>;
}
