// "Link to…": a session, and the ones that go with it (its sub-agents, and any unsorted sessions
// in its folder the person ticks), to a workstream, a project (its main workstream), or a
// workstream made here. Explicit links are written through the API, one per session; the event
// stream refreshes the session lists.

import { useMutation } from '@tanstack/react-query';
import { useId, useState } from 'react';
import { ApiError, useApi, useProjects, useSessions, useTasks, useWorkstreams, type Session } from '../data/index.ts';
import { Button, Dialog, DialogContent, DialogFooter } from '../design/index.ts';
import { sessionTitle } from './format.ts';
import { companions, defaultWorkstream, newDefaultWorkstream, parseTarget } from './link-targets.ts';

export function LinkSessionDialog({ session, onClose }: { session: Session; onClose: () => void }) {
  const api = useApi();
  const projects = useProjects();
  const workstreams = useWorkstreams();
  const tasks = useTasks();
  const sessions = useSessions();
  const id = useId();
  const [target, setTarget] = useState(session.workstream ?? '');
  const [task, setTask] = useState(session.task ?? '');
  const [newName, setNewName] = useState('');
  const { subagents, unsorted } = companions(session, sessions.data ?? []);
  // Sub-agents go with their parent unless unticked; other sessions only when ticked.
  const [left, setLeft] = useState<ReadonlySet<string>>(() => new Set());
  const [added, setAdded] = useState<ReadonlySet<string>>(() => new Set());
  const chosen = [session, ...subagents.filter((s) => !left.has(s.id)), ...unsorted.filter((s) => added.has(s.id))];
  const parsed = parseTarget(target);
  const workstreamId = parsed?.kind === 'workstream' ? parsed.id : undefined;

  const link = useMutation({
    mutationFn: async () => {
      if (parsed === undefined) throw new Error('Choose where to link it.');
      let workstream: string;
      if (parsed.kind === 'workstream') {
        workstream = parsed.id;
      } else if (parsed.kind === 'project') {
        const project = projects.data?.find((p) => p.id === parsed.id);
        if (project === undefined) throw new Error('That project is gone; choose again.');
        const existing = defaultWorkstream(project, workstreams.data ?? []);
        workstream = existing?.id ?? (await api.createWorkstream(newDefaultWorkstream(project))).id;
      } else {
        const name = newName.trim();
        if (name === '') throw new Error('Name the new workstream.');
        workstream = (await api.createWorkstream({ project: parsed.project, name })).id;
      }
      const withTask = parsed.kind === 'workstream' && task !== '' ? { task } : {};
      for (const s of chosen) await api.linkSession(s.id, { workstream, ...withTask });
    },
    onSuccess: onClose,
  });
  const queries = [projects, workstreams, tasks];
  const loading = queries.some((q) => q.isPending);
  const failed = queries.some((q) => q.error !== null);
  const choices = (tasks.data ?? []).filter((t) => t.workstream === workstreamId);
  const selectClass = 'w-full rounded-md border border-line bg-card px-3 py-2 text-sm text-ink';
  const busy = link.isPending;
  const flip = (set: (f: (s: ReadonlySet<string>) => ReadonlySet<string>) => void, sid: string) =>
    set((current) => {
      const next = new Set(current);
      if (!next.delete(sid)) next.add(sid);
      return next;
    });
  const label = chosen.length === 1 ? 'Link session' : `Link ${chosen.length} sessions`;
  const error = link.error;
  return (
    <Dialog open onOpenChange={(open) => { if (!open && !busy) onClose(); }}>
      <DialogContent
        title="Link session"
        description="Choose a workstream (or a project, for its main workstream) and, optionally, a task."
      >
        <form onSubmit={(event) => { event.preventDefault(); link.mutate(); }}>
          <div className="flex flex-col gap-4 p-4">
            {loading && <p role="status">Loading work…</p>}
            {failed && <p role="alert">Could not load workstreams or tasks. Close and try again.</p>}
            <label htmlFor={`${id}-workstream`} className="flex flex-col gap-1 text-sm">
              Workstream
              <select id={`${id}-workstream`} className={selectClass} value={target} required disabled={loading || failed || busy}
                onChange={(event) => { setTarget(event.target.value); setTask(''); link.reset(); }}>
                <option value="">Choose a workstream</option>
                {(projects.data ?? []).map((p) => (
                  <optgroup key={p.id} label={p.name}>
                    <option value={`project:${p.id}`}>{p.name} (its main workstream)</option>
                    {(workstreams.data ?? []).filter((w) => w.project === p.id).map((w) => (
                      <option key={w.id} value={w.id}>{p.name} · {w.name}</option>
                    ))}
                    <option value={`new:${p.id}`}>{p.name} · New workstream…</option>
                  </optgroup>
                ))}
              </select>
            </label>
            {parsed?.kind === 'new' && (
              <label htmlFor={`${id}-new`} className="flex flex-col gap-1 text-sm">
                New workstream's name
                <input id={`${id}-new`} className={selectClass} value={newName} required disabled={busy}
                  onChange={(event) => { setNewName(event.target.value); link.reset(); }} />
              </label>
            )}
            <label htmlFor={`${id}-task`} className="flex flex-col gap-1 text-sm">
              Task (optional)
              <select id={`${id}-task`} className={selectClass} value={task} disabled={workstreamId === undefined || busy || loading || failed}
                onChange={(event) => { setTask(event.target.value); link.reset(); }}>
                <option value="">No task</option>
                {choices.map((t) => <option key={t.id} value={t.id}>{t.key} · {t.title}</option>)}
              </select>
            </label>
            {(subagents.length > 0 || unsorted.length > 0) && (
              <fieldset className="flex flex-col gap-1 text-sm" disabled={busy}>
                <legend className="mb-1">Also link</legend>
                {subagents.map((s) => (
                  <label key={s.id} className="flex items-center gap-2">
                    <input type="checkbox" checked={!left.has(s.id)} onChange={() => flip(setLeft, s.id)} />
                    <span className="min-w-0 truncate">Its sub-agent “{sessionTitle(s)}”</span>
                  </label>
                ))}
                {unsorted.map((s) => (
                  <label key={s.id} className="flex items-center gap-2">
                    <input type="checkbox" checked={added.has(s.id)} onChange={() => flip(setAdded, s.id)} />
                    <span className="min-w-0 truncate">“{sessionTitle(s)}”, unsorted, in {s.cwd}</span>
                  </label>
                ))}
              </fieldset>
            )}
            {error !== null && (
              <p role="alert" className="text-sm text-risk">
                {error instanceof ApiError || error instanceof Error ? error.message : 'Could not link the session. Try again.'}
              </p>
            )}
          </div>
          <DialogFooter>
            <Button type="button" variant="ghost" disabled={busy} onClick={onClose}>Cancel</Button>
            <Button type="submit" disabled={target === '' || loading || failed || busy}>{busy ? 'Linking…' : label}</Button>
          </DialogFooter>
        </form>
      </DialogContent>
    </Dialog>
  );
}
