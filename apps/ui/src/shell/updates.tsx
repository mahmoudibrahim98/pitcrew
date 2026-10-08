import { createContext, useContext, useEffect, useRef, useState, type ReactNode } from 'react';
import { Button, Dialog, DialogContent, DialogFooter } from '../design/index.ts';
import { Page } from './pages/page.tsx';

interface Status { enabled: boolean; prereleases: boolean; version?: string; notesUrl?: string }
interface UpdateState { status: Status | undefined; busy: boolean; message: string; run(command: string, args?: Record<string, unknown>): Promise<void> }
const Updates = createContext<UpdateState | null>(null);
const invoke = async <T,>(command: string, args?: Record<string, unknown>): Promise<T> =>
  (await import('@tauri-apps/api/core')).invoke<T>(command, args);

export function DesktopUpdates({ children }: { children: ReactNode }) {
  const [status, setStatus] = useState<Status>();
  const [busy, setBusy] = useState(false);
  const [message, setMessage] = useState('');
  const [dismissed, setDismissed] = useState<string>();
  const [confirm, setConfirm] = useState<string>();
  const running = useRef(false);
  useEffect(() => {
    let stopped = false;
    let unlisten: (() => void) | undefined;
    void (async () => {
      const { listen } = await import('@tauri-apps/api/event');
      const off = await listen<Status>('gateway://update', (e) => { if (!stopped) setStatus(e.payload); });
      if (stopped) { off(); return; }
      unlisten = off;
      const initial = await invoke<Status>('gateway_update_status');
      if (!stopped) setStatus(initial);
    })().catch(() => { if (!stopped) setMessage('Update status is unavailable.'); });
    return () => { stopped = true; unlisten?.(); };
  }, []);
  const run: UpdateState['run'] = async (command, args) => {
    if (running.current) return;
    running.current = true;
    setBusy(true);
    setMessage('');
    try {
      const next = await invoke<Status | undefined>(command, args);
      if (next !== undefined && command !== 'gateway_update_notes') {
        setStatus(next);
        if (next.version === undefined) setMessage(next.enabled ? 'You are up to date.' : 'Automatic updates are disabled in this build or installation.');
        if (command === 'gateway_update_check') setDismissed(undefined);
      }
    } catch (error) { setMessage(typeof error === 'string' ? error : 'The update operation failed. Try again.'); }
    finally { running.current = false; setBusy(false); }
  };
  return <Updates value={{ status, busy, message, run }}>
    {children}
    {status?.version !== undefined && status.version !== dismissed && <aside aria-label="Desktop update" className="fixed right-4 bottom-4 z-30 max-w-sm rounded-lg border border-line bg-card p-4 text-sm text-ink shadow-pop">
      <p>PitCrew {status.version} is available.</p>
      <div className="mt-2 flex gap-2">
        <a
          href={status.notesUrl}
          aria-disabled={busy}
          className="inline-flex items-center font-medium hover:underline"
          onClick={(event) => {
            event.preventDefault();
            if (!busy) void run('gateway_update_notes', { version: status.version });
          }}
        >Release notes</a>
        <Button disabled={busy} onClick={() => setConfirm(status.version)}>Update</Button>
        <Button disabled={busy} onClick={() => setDismissed(status.version)}>Later</Button>
      </div>
      {message && <p role="status">{message}</p>}
    </aside>}
    <Dialog open={confirm !== undefined} onOpenChange={(open) => { if (!open && !busy) setConfirm(undefined); }}>
      <DialogContent title="Install update?" description={`Install PitCrew ${confirm ?? ''} and restart the app? Save your work before continuing.`} showClose={!busy}>
        {message && <p role="status" className="p-4 text-sm">{message}</p>}
        <DialogFooter>
          <Button disabled={busy} onClick={() => setConfirm(undefined)}>Cancel</Button>
          <Button variant="primary" disabled={busy || confirm !== status?.version} onClick={() => void run('gateway_update_install', { version: confirm })}>{busy ? 'Updating…' : 'Install and restart'}</Button>
        </DialogFooter>
      </DialogContent>
    </Dialog>
  </Updates>;
}

export function UpdateSettings({ embedded = false }: { embedded?: boolean } = {}) {
  const updates = useContext(Updates);
  const content = <section className="space-y-3 text-sm">
      <h2 className="font-semibold">Desktop updates</h2>
      {updates === null ? <p>Updates are managed by the desktop app.</p> : <>
        <p>Checks run on startup and daily. Updates are installed only after you agree.</p>
        {updates.status?.enabled === false && <p>Automatic updates are disabled in this build or installation. On Linux, deb/rpm installations use the package manager.</p>}
        <Button disabled={updates.busy || updates.status?.enabled !== true} onClick={() => void updates.run('gateway_update_check')}>Check now</Button>
        <label className="flex items-center gap-2"><input type="checkbox" checked={updates.status?.prereleases ?? false} disabled={updates.busy || updates.status === undefined} onChange={(e) => void updates.run('gateway_update_channel', { prereleases: e.target.checked })} />Include pre-releases</label>
        {updates.status?.version !== undefined && <p>Version {updates.status.version} is available.</p>}
        {updates.message && <p role="status">{updates.message}</p>}
      </>}
    </section>;
  return embedded ? content : <Page title="Settings" placeholder={false}>{content}</Page>;
}
