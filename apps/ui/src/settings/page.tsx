import { useMemo, useState, type FormEvent, type ReactNode } from 'react';
import { Link, useLocation } from '@tanstack/react-router';
import { useQueryClient } from '@tanstack/react-query';
import { Avatar, Button, Kbd, ThemeToggle, initials } from '../design/index.ts';
import { keys, useApi, useGatewayWorkspaces, useLiveQuery, useMachines, useMe, useMembers, useRemoteGateway, useWorkspace, type Member, type Persona } from '../data/index.ts';
import { isDesktop } from '../data/transport.ts';
import { createHubOnboardingApi } from '../onboarding/hub-api.ts';
import type { HooksDiff, OnboardingApi, SafetySettings } from '../onboarding/api.ts';
import { CheckRowLine, InstallPageNote } from '../onboarding/check-rows.tsx';
import { SignInPanel } from '../onboarding/sign-in-panel.tsx';
import { hookDiff } from '../onboarding/hook-diff.ts';
import { Page } from '../shell/pages/page.tsx';
import { paths } from '../shell/paths.ts';
import { useWorkspaceId } from '../shell/layout.ts';
import { RemoveWorkspaceDialog } from '../shell/remove-workspace.tsx';
import { SHORTCUTS } from '../shell/shortcuts.ts';
import { UpdateSettings } from '../shell/updates.tsx';
import { IntegrationsPage } from '../projects/integrations/integrations-page.tsx';
import { SECTIONS } from './sections.ts';
import { useAppearance } from './appearance.ts';
import { version as uiVersion } from '../../package.json';

const FIELD = 'block w-full max-w-lg rounded-sm border border-line-2 bg-card px-2 py-1.5 text-sm text-ink';
function message(error: unknown) { return error instanceof Error ? error.message : 'The operation failed. Try again.'; }
function useAction() {
  const [busy, setBusy] = useState(false);
  const [status, setStatus] = useState('');
  const [error, setError] = useState('');
  async function run(action: () => Promise<unknown>, success = 'Saved.') {
    if (busy) return;
    setBusy(true); setStatus(''); setError('');
    try { await action(); setStatus(success); } catch (e) { setError(message(e)); }
    finally { setBusy(false); }
  }
  return { busy, run, feedback: <>{status && <p role="status">{status}</p>}{error && <p role="alert">{error}</p>}</> };
}
function Field({ label, name, value, type = 'text', ...props }: { label: string; name: string; value?: string | number; type?: string; required?: boolean; maxLength?: number; min?: number; max?: number }) {
  return <label className="block space-y-1"><span>{label}</span><input className={FIELD} name={name} type={type} defaultValue={value} {...props} /></label>;
}
function Form({ children, submit, busy }: { children: ReactNode; submit(data: FormData): void; busy: boolean }) {
  return <form className="space-y-4" onSubmit={(e: FormEvent<HTMLFormElement>) => { e.preventDefault(); if (!busy) submit(new FormData(e.currentTarget)); }}><fieldset disabled={busy} className="space-y-4">{children}<Button type="submit" variant="primary">{busy ? 'Saving…' : 'Save changes'}</Button></fieldset></form>;
}
function text(data: FormData, key: string) { return String(data.get(key) ?? ''); }
function Profile({ member }: { member: Member }) {
  const api = useApi(); const action = useAction();
  return <><Avatar member={member} size="lg" /><Form busy={action.busy} submit={(data) => void action.run(() => api.saveProfile({ name: text(data, 'name'), handle: text(data, 'handle'), avatar: { initials: text(data, 'initials'), colour: text(data, 'colour') } }))}>
    <Field label="Name" name="name" value={member.name} required maxLength={160} />
    <Field label="Handle" name="handle" value={member.handle} required maxLength={33} />
    <Field label="Avatar initials" name="initials" value={member.avatar?.initials ?? initials(member.name)} required maxLength={8} />
    <Field label="Avatar colour" name="colour" type="color" value={member.avatar?.colour ?? '#485c87'} />
  </Form>{action.feedback}</>;
}
function Workspace() {
  const api = useApi(); const queryClient = useQueryClient(); const action = useAction();
  const workspace = useWorkspace(); const members = useMembers();
  const info = useLiveQuery({ queryKey: ['settings'], queryFn: ({ signal }) => api.settings(signal) });
  const me = useMe().data; const owner = members.data?.find((m) => m.kind === 'human');
  const [copied, setCopied] = useState('');
  return <>
    <p>Owner: {owner?.name ?? 'Loading…'} {owner?.handle}</p>
    {workspace.data !== undefined && me?.id === owner?.id ? <Form busy={action.busy} submit={(data) => void action.run(async () => { await api.renameWorkspace(text(data, 'name')); await queryClient.invalidateQueries({ queryKey: keys.workspace }); })}>
      <Field label="Workspace name" name="name" value={workspace.data.workspace.name} required maxLength={160} />
    </Form> : <p>Workspace name: {workspace.data?.workspace.name}. Only its owner can change it.</p>}
    {info.data && <div><p>Data folder (read-only)</p><code className="break-all">{info.data.data_folder}</code><Button onClick={() => void action.run(async () => { if (!navigator.clipboard) throw new Error('Clipboard is unavailable. Select and copy the folder path.'); await navigator.clipboard.writeText(info.data.data_folder); setCopied('Copied'); }, 'Data folder copied.')}>{copied || 'Copy data folder'}</Button></div>}
    {info.error && <><p role="alert">{message(info.error)}</p><Button onClick={() => void info.refetch()}>Retry metadata</Button></>}{action.feedback}
  </>;
}
function ConnectedMachines() {
  const ws = useWorkspaceId(); const gateways = useGatewayWorkspaces(); const remote = useRemoteGateway(); const [removing, setRemoving] = useState<string>();
  const selected = gateways?.list?.find((w) => w.id === removing);
  return <>{gateways?.list?.map((w) => <div key={w.id} className="flex flex-wrap items-center gap-2 rounded-sm border border-line p-3">
    <Link to={paths.under(w.id, 'settings/machines')}>{w.name} {w.kind === 'local' ? '(this computer)' : '(connected machine)'}</Link><span>{w.state}</span>
    {w.kind === 'remote' && remote !== null && <Button onClick={() => setRemoving(w.id)}>Remove {w.name}…</Button>}
  </div>)}
    {remote !== null && <Link to={paths.connect()}>Connect a machine…</Link>}
    {selected && remote !== null && <RemoveWorkspaceDialog workspace={selected} remote={remote} onClose={() => setRemoving(undefined)} returnFocus={() => document.querySelector<HTMLAnchorElement>(`a[href="${paths.under(ws, 'settings/machines')}"]`)?.focus()} />}
  </>;
}
function MachineCheck({ api }: { api: OnboardingApi }) {
  const check = useLiveQuery({ queryKey: ['settings-machine-check'], queryFn: () => api.checkMachine({ kind: 'local' }) });
  const [fix, setFix] = useState<string>();
  return <>
    <Button disabled={check.isFetching} onClick={() => void check.refetch()}>Check again</Button>
    {check.isPending && <p>Checking this workspace’s machine…</p>}
    {check.error && <p role="alert">{message(check.error)}</p>}
    <ul className="divide-y divide-line">{check.data?.rows.map((row) => <CheckRowLine key={row.id} row={row} note={fix === row.id ? <InstallPageNote row={row} /> : undefined} action={row.fixable ? <Button onClick={() => setFix(row.id)}>Review fix</Button> : undefined} />)}</ul>
  </>;
}
function AgentRecipe({ persona }: { persona: Persona }) {
  const api = useApi(); const action = useAction();
  return <section className="space-y-3 rounded-sm border border-line p-3"><h3 className="font-semibold">{persona.name}</h3><Form busy={action.busy} submit={(data) => void action.run(() => api.savePersona({ ...persona, name: text(data, 'name'), engine: text(data, 'engine') as Persona['engine'], model: text(data, 'model'), instructions: text(data, 'instructions'), permission_mode: text(data, 'permission_mode') as Persona['permission_mode'] }))}>
    <Field label="Default agent name" name="name" value={persona.name} required />
    <label className="block">CLI<select name="engine" defaultValue={persona.engine} className={FIELD}><option value="claude">Claude Code</option><option value="codex">Codex</option><option value="opencode">OpenCode</option></select></label>
    <Field label="Model (blank uses the CLI default)" name="model" value={persona.model ?? ''} maxLength={200} />
    <label className="block">Standing instructions<textarea name="instructions" className={FIELD} rows={4} maxLength={32000} defaultValue={persona.instructions ?? ''} /></label>
    <PermissionSelect value={persona.permission_mode} />
  </Form>{action.feedback}</section>;
}
function PermissionSelect({ value }: { value: string }) {
  return <label className="block">Default permission mode<select name="permission_mode" defaultValue={value} className={FIELD}><option value="default">Ask before risky actions</option><option value="plan">Plan before making changes</option><option value="accept_edits">Accept file edits; ask before commands</option></select></label>;
}
function Agents({ api }: { api: OnboardingApi }) {
  const client = useApi(); const me = useMe(); const members = useMembers();
  const personas = useLiveQuery({ queryKey: keys.personas, queryFn: ({ signal }) => client.personas(signal) });
  return <><ConnectedMachines /><MachineCheck api={api} /><h3 className="font-semibold">CLI accounts on this workspace’s machine</h3><SignInPanel api={api} target={{ kind: 'local' }} />
    <h3 className="font-semibold">Default agents</h3><p>These recipes apply to new sessions. Existing sessions keep their current settings.</p>
    {personas.error && <><p role="alert">{message(personas.error)}</p><Button onClick={() => void personas.refetch()}>Retry default agents</Button></>}
    {personas.data?.length === 0 && <p>No default agents yet. Add an agent from + New.</p>}
    {personas.data?.map((p) => !members.data?.some((m) => m.persona === p.id && m.owner !== me.data?.id) ? <AgentRecipe key={p.id} persona={p} /> : <p key={p.id}>{p.name} ({p.engine}); only the owning person can edit defaults.</p>)}
  </>;
}
function Hooks({ api }: { api: OnboardingApi }) {
  const action = useAction(); const [preview, setPreview] = useState<HooksDiff>();
  const status = useLiveQuery({ queryKey: ['settings-hooks'], queryFn: () => api.hooksDiff() });
  return <>
    <p>Review the exact configuration diff before installing hooks on this workspace’s machine.</p>
    {status.error && <p role="alert">{message(status.error)}</p>}
    {status.data?.engines.map((engine) => <p key={engine.engine}>{engine.engine}: {engine.status}. {engine.detail}</p>)}
    {status.data?.engines.length === 0 && <p>No supported agent CLIs were found.</p>}
    <Button disabled={action.busy} onClick={() => void action.run(async () => { setPreview(undefined); setPreview(await api.hooksDiff()); }, 'Review the diff below.')}>Review and install</Button>
    {preview?.files.map((file) => <div key={file.path}><p className="break-all">{file.path}</p><pre tabIndex={0} aria-label={`Diff: ${file.path}`} className="overflow-auto rounded-sm border border-line p-3 text-xs">{hookDiff(file.path, file.before, file.after)}</pre></div>)}
    {preview?.engines.filter((e) => e.status === 'conflicting').map((e) => <p role="alert" key={e.engine}>{e.engine}: {e.detail}</p>)}
    {preview?.files.length === 0 && <p>No installation changes are needed.</p>}
    {preview !== undefined && preview.files.length > 0 && <Button variant="primary" disabled={action.busy} onClick={() => void action.run(async () => { await api.installHooks(preview); setPreview(undefined); await status.refetch(); }, 'Hooks installed. Conflicting engines were skipped.')}>Install reviewed hooks</Button>}
    {action.feedback}
  </>;
}
function SafetyForm({ api, value }: { api: OnboardingApi; value: SafetySettings }) {
  const action = useAction();
  return <><Form busy={action.busy} submit={(data) => void action.run(() => api.saveSafety({ permissionMode: text(data, 'permission_mode').replaceAll('_', '-') as SafetySettings['permissionMode'], backOfficeEnabled: data.has('automatic'), backOfficeCaps: { maxAutoAcceptPerHour: Number(text(data, 'cap')) } }))}>
    <PermissionSelect value={value.permissionMode.replaceAll('-', '_')} />
    <p>Changes apply to new sessions. Running sessions keep their permission mode.</p>
    <label><input name="automatic" type="checkbox" defaultChecked={value.backOfficeEnabled} /> Let PitCrew accept low-risk changes automatically</label>
    <Field name="cap" label="Up to this many an hour (0 turns automatic acceptance off)" type="number" min={0} max={100} value={value.backOfficeCaps.maxAutoAcceptPerHour} required />
  </Form>{action.feedback}</>;
}
function Safety({ api }: { api: OnboardingApi }) {
  const settings = useLiveQuery({ queryKey: ['safety'], queryFn: () => api.readSafety() });
  return <>{settings.isPending && <p>Loading safety settings…</p>}{settings.error && <><p role="alert">{message(settings.error)}</p><Button onClick={() => void settings.refetch()}>Retry safety</Button></>}{settings.data && <SafetyForm api={api} value={settings.data} />}</>;
}
function Appearance() {
  const density = useAppearance((s) => s.density); const setDensity = useAppearance((s) => s.setDensity);
  return <><h3>Theme</h3><ThemeToggle /><label className="block">Density<select className={FIELD} value={density} onChange={(e) => setDensity(e.target.value as typeof density)}><option value="comfortable">Comfortable</option><option value="compact">Compact</option></select></label><p>Theme and density apply immediately and are saved on this device.</p></>;
}
function About() {
  const api = useApi(); const info = useLiveQuery({ queryKey: ['settings-about'], queryFn: ({ signal }) => api.request<{ version: string; protocol: number }>('GET', '/v1/host/info', { signal }) });
  const app = useLiveQuery({ queryKey: ['app-version'], queryFn: async () => isDesktop() ? (await import('@tauri-apps/api/app')).getVersion() : `${uiVersion} (browser development build)` });
  return <><p>App: {app.data ?? 'Loading…'}</p>{app.error && <p role="alert">{message(app.error)}</p>}
    {info.data && <><p>Daemon: {info.data.version}</p><p>Protocol: {info.data.protocol}</p><p>Logs: Daemon logs go to stderr; the launcher captures them.</p></>}
    {info.error && <p role="alert">{message(info.error)}</p>}
  </>;
}

export function SettingsPage() {
  const location = useLocation(); const ws = useWorkspaceId(); const client = useApi(); const me = useMe();
  const section = location.pathname.split('/').at(-1) === 'settings' ? 'profile' : location.pathname.split('/').at(-1);
  const api = useMemo(() => createHubOnboardingApi({ data: client }), [client]);
  const visible = SECTIONS.filter(([id]) => id !== 'updates' || isDesktop());
  const title = visible.find(([id]) => id === section)?.[1];
  let content: ReactNode;
  switch (section) {
    case 'profile': content = me.data ? <Profile key={me.data.id} member={me.data} /> : <>{me.error ? <><p role="alert">{message(me.error)}</p><Button onClick={() => void me.refetch()}>Retry profile</Button></> : <p>Loading profile…</p>}</>; break;
    case 'workspace': content = <Workspace />; break;
    case 'machines': content = <><ConnectedMachines /><h3>This workspace’s machine</h3><MachineCheck api={api} /><MachineList /></>; break;
    case 'agents': content = <Agents api={api} />; break;
    case 'hooks': content = <Hooks api={api} />; break;
    case 'safety': content = <Safety api={api} />; break;
    case 'integrations': content = <IntegrationsPage />; break;
    case 'appearance': content = <Appearance />; break;
    case 'keyboard-shortcuts': content = <ul className="space-y-3">{Object.entries(SHORTCUTS).map(([action, shortcut]) => <li key={action}>{({ layout: 'Switch layout', palette: 'Open command palette', orchestrator: 'Show or hide the Orchestrator', sidebar: 'Collapse or expand sidebar' })[action as keyof typeof SHORTCUTS]} <Kbd keys={shortcut} /></li>)}</ul>; break;
    case 'updates': content = isDesktop() ? <UpdateSettings embedded /> : <p>Updates are managed by the desktop app.</p>; break;
    case 'about': content = <About />; break;
    default: content = <p>Unknown Settings section. Choose a section from the list.</p>;
  }
  return <Page title="Settings" placeholder={false}><div className="grid gap-6 md:grid-cols-[11rem_minmax(0,1fr)]"><nav aria-label="Settings sections" className="flex flex-col gap-1">{visible.map(([id, label]) => <Link key={id} to={paths.under(ws, `settings/${id}`)} className="rounded-sm px-2 py-1 text-sm hover:bg-hover" aria-current={id === section ? 'page' : undefined}>{label}</Link>)}</nav><section aria-label={title ?? 'Settings section'} className="min-w-0 space-y-4 text-sm" data-settings-content><h2 className="text-lg font-semibold">{title ?? 'Unknown section'}</h2>{content}</section></div></Page>;
}
function MachineList() {
  const machines = useMachines();
  const me = useMe(); const members = useMembers();
  const owner = members.data?.find((m) => m.kind === 'human');
  return <>{machines.error && <p role="alert">{message(machines.error)}</p>}{machines.data?.map((m) => <section key={m.id} className="rounded-sm border border-line p-3"><p>{m.name}: {m.kind}, {m.liveness}{m.kind !== 'local' ? '. Open its connected workspace above to check tools and sign in.' : ''}</p>{me.data?.id === owner?.id && <MachineName id={m.id} name={m.name} />}</section>)}</>;
}
function MachineName({ id, name }: { id: string; name: string }) {
  const api = useApi(); const action = useAction();
  return <><Form busy={action.busy} submit={(data) => void action.run(() => api.renameMachine(id, text(data, 'name')))}><Field name="name" label="Machine name" value={name} required maxLength={120} /></Form>{action.feedback}</>;
}
