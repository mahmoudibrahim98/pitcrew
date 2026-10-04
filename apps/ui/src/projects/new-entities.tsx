// The work model owns these + New entries. Mutations refresh their lists immediately, while
// the stream also refreshes other clients. The shell owns modal/focus handling.
import { useMutation, useQueryClient } from '@tanstack/react-query';
import { useParams, useRouter } from '@tanstack/react-router';
import { useState, type FormEvent, type ReactNode } from 'react';
import { keys, useApi, useLiveQuery, useMachines, useMe, useMembers, useProjects, type Engine, type Machine, type PermissionMode } from '../data/index.ts';
import { Button, DialogFooter } from '../design/index.ts';
import { paths, useWorkspaceId } from '../shell/index.ts';
import { ErrorNote, Field, inputClass } from './ui.tsx';

export function usePersonas() {
  const api = useApi();
  return useLiveQuery({ queryKey: keys.personas, queryFn: ({ signal }) => api.personas(signal) });
}
export function useTeams() {
  const api = useApi();
  return useLiveQuery({ queryKey: keys.teams, queryFn: ({ signal }) => api.teams(signal) });
}

export function suggestedKey(name: string): string {
  const letters = name.toUpperCase().replace(/[^A-Z0-9]/g, '').replace(/^[0-9]+/, '').slice(0, 10);
  return letters.length === 1 ? `${letters}P` : letters;
}

/** Validate against the selected machine, never the browser's operating system. */
export function absoluteRoot(path: string, machine: Machine): boolean {
  if (path.includes('\0')) return false;
  if (machine.kind === 'wsl') return path.startsWith('/');
  const os = machine.info?.os.toLowerCase();
  if (os === 'windows') return /^[A-Za-z]:[\\/]/.test(path) || /^\\\\[^\\/]+\\[^\\/]+(?:\\|$)/.test(path);
  if (os !== undefined) return path.startsWith('/');
  // Before a runner reports its platform, accept only unambiguously absolute forms.
  return path.startsWith('/') || /^[A-Za-z]:[\\/]/.test(path) || /^\\\\[^\\/]+\\[^\\/]+(?:\\|$)/.test(path);
}
function Form({ children, submit, pending, error, valid = true }: {
  children: ReactNode; submit(e: FormEvent): void; pending: boolean; error: Error | null; valid?: boolean;
}) {
  return <form onSubmit={submit} className="flex min-h-0 flex-col gap-3 overflow-y-auto px-4 py-4">
    {children}
    {error !== null && <ErrorNote error={error} what="save" />}
    <DialogFooter><Button type="submit" variant="primary" disabled={pending || !valid}>{pending ? 'Saving…' : 'Create'}</Button></DialogFooter>
  </form>;
}

export function NewProjectDialog({ close }: { close(): void }) {
  const api = useApi();
  const client = useQueryClient();
  const router = useRouter();
  const ws = useWorkspaceId();
  const machines = useMachines();
  const [name, setName] = useState('');
  const [key, setKey] = useState<string | null>(null);
  const [machineId, setMachineId] = useState('');
  const [path, setPath] = useState('');
  const [first, setFirst] = useState('');
  const [rootError, setRootError] = useState<string | null>(null);
  const selected = machineId || machines.data?.[0]?.id || '';
  const machine = machines.data?.find((m) => m.id === selected);
  const projectKey = key ?? suggestedKey(name);
  const create = useMutation({ mutationFn: () => api.createProject({ name: name.trim(), key: projectKey,
    root: { machine: selected, path }, ...(first.trim() === '' ? {} : { first_workstream: first.trim() }) }),
    onSuccess: async (project) => {
      await Promise.all([client.invalidateQueries({ queryKey: keys.projects.lists }), client.invalidateQueries({ queryKey: keys.workstreams.lists })]);
      close();
      void router.navigate({ href: paths.project(ws, project.id) });
    } });
  const submit = (e: FormEvent) => {
    e.preventDefault();
    if (machine === undefined || !absoluteRoot(path, machine)) { setRootError('Enter an absolute path for this machine.'); return; }
    setRootError(null);
    create.mutate();
  };
  return <Form submit={submit} pending={create.isPending} error={create.error} valid={name.trim() !== '' && selected !== ''}>
    <Field label="Name">{(id) => <input id={id} required value={name} onChange={(e) => setName(e.target.value)} className={inputClass} />}</Field>
    <Field label="Key" hint="2–10 uppercase letters or digits, starting with a letter.">{(id) => <input id={id} required pattern="[A-Z][A-Z0-9]{1,9}" value={projectKey} onChange={(e) => setKey(e.target.value.toUpperCase())} className={inputClass} />}</Field>
    <Field label="Machine">{(id) => <select id={id} required value={selected} onChange={(e) => { setMachineId(e.target.value); setRootError(null); }} className={inputClass}>
      {(machines.data ?? []).map((m) => <option key={m.id} value={m.id}>{m.name}</option>)}
    </select>}</Field>
    <Field label="Root path">{(id) => <input id={id} required value={path} aria-invalid={rootError !== null || undefined} onChange={(e) => { setPath(e.target.value); setRootError(null); }} className={inputClass} />}</Field>
    {rootError !== null && <p role="alert" className="text-sm text-risk">{rootError}</p>}
    {machines.error !== null && <ErrorNote error={machines.error} what="load machines" />}
    <Field label="First workstream (optional)">{(id) => <input id={id} value={first} onChange={(e) => setFirst(e.target.value)} className={inputClass} />}</Field>
  </Form>;
}

export function NewWorkstreamDialog({ close }: { close(): void }) {
  const { project: context }: { project?: string } = useParams({ strict: false });
  const api = useApi();
  const client = useQueryClient();
  const router = useRouter();
  const ws = useWorkspaceId();
  const projects = useProjects();
  const [project, setProject] = useState(context ?? '');
  const [name, setName] = useState('');
  const selected = project || projects.data?.[0]?.id || '';
  const create = useMutation({ mutationFn: () => api.createWorkstream({ project: selected, name: name.trim() }),
    onSuccess: async (workstream) => {
      await client.invalidateQueries({ queryKey: keys.workstreams.lists });
      close();
      void router.navigate({ href: paths.workstream(ws, workstream.project, workstream.id) });
    } });
  return <Form submit={(e) => { e.preventDefault(); create.mutate(); }} pending={create.isPending} error={create.error} valid={name.trim() !== '' && selected !== ''}>
    <Field label="Name">{(id) => <input id={id} required value={name} onChange={(e) => setName(e.target.value)} className={inputClass} />}</Field>
    <Field label="Project">{(id) => <select id={id} required value={selected} onChange={(e) => setProject(e.target.value)} className={inputClass}>
      {(projects.data ?? []).map((p) => <option key={p.id} value={p.id}>{p.name}</option>)}
    </select>}</Field>
    {projects.error !== null && <ErrorNote error={projects.error} what="load projects" />}
  </Form>;
}

export function NewAgentDialog({ close }: { close(): void }) {
  const api = useApi();
  const client = useQueryClient();
  const [name, setName] = useState('');
  const [engine, setEngine] = useState<Engine>('claude');
  const [model, setModel] = useState('');
  const [instructions, setInstructions] = useState('');
  const [permission, setPermission] = useState<PermissionMode>('default');
  const create = useMutation({ mutationFn: () => api.createPersona({ name, engine, permission_mode: permission,
    ...(model.trim() === '' ? {} : { model: model.trim() }), ...(instructions === '' ? {} : { instructions }) }),
    onSuccess: async () => {
      await Promise.all([client.invalidateQueries({ queryKey: keys.personas }), client.invalidateQueries({ queryKey: keys.members })]);
      close();
    } });
  return <Form submit={(e) => { e.preventDefault(); create.mutate(); }} pending={create.isPending} error={create.error} valid={name.trim() !== ''}>
    <Field label="Name">{(id) => <input id={id} required maxLength={80} value={name} onChange={(e) => setName(e.target.value)} className={inputClass} />}</Field>
    <Field label="Engine">{(id) => <select id={id} value={engine} onChange={(e) => setEngine(e.target.value as Engine)} className={inputClass}>{['claude', 'codex', 'opencode'].map((engine) => <option key={engine}>{engine}</option>)}</select>}</Field>
    <Field label="Model (optional)">{(id) => <input id={id} maxLength={200} value={model} onChange={(e) => setModel(e.target.value)} className={inputClass} />}</Field>
    <Field label="Instructions (optional)">{(id) => <textarea id={id} maxLength={32000} value={instructions} onChange={(e) => setInstructions(e.target.value)} className={inputClass} />}</Field>
    <Field label="Permission mode">{(id) => <select id={id} value={permission} onChange={(e) => setPermission(e.target.value as PermissionMode)} className={inputClass}>
      <option value="default">Ask before actions (default)</option><option value="accept_edits">Accept edits</option><option value="plan">Plan only</option><option value="bypass_permissions">Bypass permissions</option>
    </select>}</Field>
    {permission === 'bypass_permissions' && <p className="text-sm text-risk">Agents using this recipe skip CLI permission prompts.</p>}
  </Form>;
}

export function NewTeamDialog({ close }: { close(): void }) {
  const api = useApi();
  const client = useQueryClient();
  const members = useMembers();
  const me = useMe();
  const [name, setName] = useState('');
  const [lead, setLead] = useState('');
  const [selected, setSelected] = useState<string[]>([]);
  const leadId = lead || me.data?.id || members.data?.[0]?.id || '';
  const create = useMutation({ mutationFn: () => api.createTeam({ name, lead: leadId, members: selected }),
    onSuccess: async () => { await client.invalidateQueries({ queryKey: keys.teams }); close(); } });
  return <Form submit={(e) => { e.preventDefault(); create.mutate(); }} pending={create.isPending} error={create.error} valid={name.trim() !== '' && leadId !== ''}>
    <Field label="Name">{(id) => <input id={id} required maxLength={80} value={name} onChange={(e) => setName(e.target.value)} className={inputClass} />}</Field>
    <Field label="Lead">{(id) => <select id={id} required value={leadId} onChange={(e) => setLead(e.target.value)} className={inputClass}>{(members.data ?? []).map((m) => <option key={m.id} value={m.id}>{m.name} ({m.handle})</option>)}</select>}</Field>
    <fieldset className="flex flex-col gap-2"><legend className="text-xs font-medium text-ink-2">Members (lead included automatically)</legend>
      {(members.data ?? []).map((m) => <label key={m.id} className="flex items-center gap-2 text-sm"><input type="checkbox" checked={m.id === leadId || selected.includes(m.id)} disabled={m.id === leadId} onChange={(e) => setSelected(e.target.checked ? [...selected, m.id] : selected.filter((id) => id !== m.id))} />{m.name} {m.kind === 'agent' ? '(agent)' : '(person)'}</label>)}
    </fieldset>
    {members.error !== null && <ErrorNote error={members.error} what="load members" />}
  </Form>;
}
