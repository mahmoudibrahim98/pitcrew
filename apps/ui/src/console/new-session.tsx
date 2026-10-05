import { useMutation, useQueryClient } from '@tanstack/react-query';
import { useRouter } from '@tanstack/react-router';
import { useId, useState } from 'react';
import { ApiError, keys, useApi, useLiveQuery, useMachines, type Engine, type Location, type PermissionMode, type WorkstreamId } from '../data/index.ts';
import { Button, DialogFooter } from '../design/index.ts';
import { paths, useWorkspaceId } from '../shell/index.ts';

const field = 'w-full rounded-md border border-line bg-card px-3 py-2 text-sm text-ink';
const modes: Record<PermissionMode, string> = { default: 'Default', plan: 'Plan', accept_edits: 'Accept edits', bypass_permissions: 'Bypass permissions' };

export function absoluteFolder(path: string, platform: 'windows' | 'unix'): boolean {
  if (/\p{Cc}/u.test(path)) return false;
  return platform === 'unix' ? path.startsWith('/') : /^[a-z]:[\\/]/iu.test(path) || /^\\\\[^\\/]+[\\/][^\\/]+/u.test(path);
}

function launchError(error: unknown, options = false): string {
  if (!(error instanceof ApiError)) return 'Could not start the session. Check the connection and try again.';
  if (options) return error.message + ' Check that this machine’s runner and terminal runtime are reachable, then retry.';
  const help = error.code === 'unavailable' ? ' Check that the machine’s runner and terminal runtime are running, and install the CLI on its PATH if missing.'
    : error.code === 'conflict' ? ' Finish the other start in this folder or choose another folder.'
    : ' Check the folder and the selected permission mode, then try again.';
  return error.message + help;
}

/** Body shared by the shell, console list and workstream entry points. */
export default function NewSession({ close, locations = [], workstream }: { close(): void; locations?: readonly Location[]; workstream?: WorkstreamId }) {
  const api = useApi();
  const cache = useQueryClient();
  const router = useRouter();
  const ws = useWorkspaceId();
  const id = useId();
  const machines = useMachines();
  const [machine, setMachine] = useState(locations[0]?.machine ?? '');
  const machineId = machine || machines.data?.find((m) => m.kind === 'local')?.id || machines.data?.[0]?.id || '';
  const [cwd, setCwd] = useState(locations[0]?.path ?? '');
  const [engine, setEngine] = useState<Engine | ''>('');
  const [mode, setMode] = useState<PermissionMode | undefined>();
  const [brief, setBrief] = useState('');
  const [title, setTitle] = useState('');
  const options = useLiveQuery({ queryKey: ['session-options', machineId], queryFn: ({ signal }) => api.sessionOptions(machineId, signal), enabled: machineId !== '' });
  const safety = useLiveQuery({ queryKey: ['safety'], queryFn: ({ signal }) => api.safety(signal) });
  const engines = options.data?.engines ?? [];
  const chosen = engines.find((e) => e.engine === engine) ?? engines[0];
  const permitted = chosen?.permission_modes ?? [];
  const preferred = mode ?? safety.data?.permission_mode ?? 'default';
  const selectedMode = permitted.includes(preferred) ? preferred : 'default';
  const promptError = (chosen?.first_prompt_forbidden ?? []).some((c) => brief.includes(c)) ? 'This Windows CLI uses a batch wrapper. Start without this first prompt and enter it in the terminal; its wrapper cannot pass control characters or " % ! ^ & | < > ( ).' : undefined;
  const valid = chosen !== undefined && options.data !== undefined && absoluteFolder(cwd, options.data.platform) && promptError === undefined && safety.data !== undefined && safety.error === null;
  const start = useMutation({
    mutationFn: () => {
      if (!valid || chosen === undefined) throw new Error('Choose an available engine and an absolute folder.');
      return api.startSession({ ...(workstream === undefined ? {} : { workstream }), machine: machineId, engine: chosen.engine, cwd, permission_mode: selectedMode,
        ...(brief === '' ? {} : { brief }), ...(title.trim() === '' ? {} : { title: title.trim() }) });
    },
    onSuccess: (session) => {
      cache.setQueryData(keys.sessions.detail(session.id), session);
      void cache.invalidateQueries({ queryKey: keys.sessions.lists });
      close();
      void router.navigate({ href: paths.session(ws, session.id) + '?view=terminal' });
    },
  });
  const busy = start.isPending;
  const changeMachine = (value: string) => { setMachine(value); setEngine(''); setMode(undefined); setCwd(locations.find((l) => l.machine === value)?.path ?? ''); start.reset(); };
  return <form onSubmit={(e) => { e.preventDefault(); if (valid && !busy) start.mutate(); }} className="flex flex-col gap-3 p-4" aria-label="New session">
    <label htmlFor={`${id}-machine`}>Machine</label>
    <select id={`${id}-machine`} className={field} value={machineId} disabled={busy || machines.isPending} onChange={(e) => changeMachine(e.target.value)}>
      {(machines.data ?? []).map((m) => <option key={m.id} value={m.id}>{m.name}</option>)}
    </select>
    {locations.length > 1 && <label>Workstream location<select className={field} value={locations.findIndex((l) => l.machine === machineId && l.path === cwd)} disabled={busy}
      onChange={(e) => { const l = locations[Number(e.target.value)]; if (l) { changeMachine(l.machine); setCwd(l.path); } }}>
      <option value={-1}>Custom folder</option>{locations.map((l, n) => <option key={n} value={n}>{l.path}</option>)}
    </select></label>}
    <label htmlFor={`${id}-cwd`}>Folder</label>
    <input id={`${id}-cwd`} className={field} value={cwd} required disabled={busy} onChange={(e) => { setCwd(e.target.value); start.reset(); }} aria-describedby={`${id}-path-note`} />
    <p id={`${id}-path-note`} className="text-xs text-ink-2">Use an absolute {options.data?.platform === 'windows' ? 'Windows path, such as C:\\work\\project' : 'path, such as /home/sam/work/project'}. The runner checks that the folder exists and is safe.</p>
    <label htmlFor={`${id}-engine`}>Engine</label>
    <select id={`${id}-engine`} className={field} value={chosen?.engine ?? ''} disabled={busy || engines.length === 0} onChange={(e) => { setEngine(e.target.value as Engine); setMode(undefined); start.reset(); }}>
      {engines.map((e) => <option key={e.engine} value={e.engine}>{e.engine === 'claude' ? 'Claude' : e.engine === 'codex' ? 'Codex' : 'OpenCode'}</option>)}
    </select>
    <label htmlFor={`${id}-mode`}>Permission mode</label>
    <select id={`${id}-mode`} className={field} value={selectedMode} disabled={busy || permitted.length === 0} onChange={(e) => { setMode(e.target.value as PermissionMode); start.reset(); }}>
      {permitted.map((m) => <option key={m} value={m}>{modes[m]}</option>)}
    </select>
    <label htmlFor={`${id}-title`}>Title (optional)</label>
    <input id={`${id}-title`} className={field} value={title} maxLength={200} disabled={busy} onChange={(e) => { setTitle(e.target.value); start.reset(); }} />
    <label htmlFor={`${id}-brief`}>First prompt (optional)</label>
    <textarea id={`${id}-brief`} className={field} value={brief} disabled={busy} onChange={(e) => { setBrief(e.target.value); start.reset(); }} />
    {options.isPending && <p role="status">Checking installed engines…</p>}
    {(options.error ?? machines.error) !== null && <p role="alert">{launchError(options.error ?? machines.error, true)} <Button type="button" onClick={() => { void machines.refetch(); void options.refetch(); }}>Retry</Button></p>}
    {options.data !== undefined && engines.length === 0 && <p role="alert">No agent CLI is available. Install Claude, Codex or OpenCode on this machine’s PATH, then retry. <Button type="button" onClick={() => { void options.refetch(); }}>Retry</Button></p>}
    {safety.error !== null && <p role="alert">Could not read the saved permission default. <Button type="button" onClick={() => { void safety.refetch(); }}>Retry</Button></p>}
    {promptError !== undefined && <p role="alert">{promptError}</p>}
    {start.error !== null && <p role="alert">{launchError(start.error)}</p>}
    <DialogFooter><Button type="button" variant="ghost" onClick={close} disabled={busy}>Cancel</Button><Button type="submit" disabled={!valid || busy || options.isFetching || options.error !== null}>{busy ? 'Starting…' : 'Start session'}</Button></DialogFooter>
  </form>;
}
