import { useMutation } from '@tanstack/react-query';
import { useId, useState } from 'react';
import { ApiError, useApi, useProjects, useTasks, useWorkstreams, type Session } from '../data/index.ts';
import { Button, Dialog, DialogContent, DialogFooter } from '../design/index.ts';

/** Explicit links are written through the API; the event stream refreshes the session lists. */
export function LinkSessionDialog({ session, onClose }: { session: Session; onClose: () => void }) {
  const api = useApi();
  const projects = useProjects();
  const workstreams = useWorkstreams();
  const tasks = useTasks();
  const id = useId();
  const [workstream, setWorkstream] = useState(session.workstream ?? '');
  const [task, setTask] = useState(session.task ?? '');
  const link = useMutation({
    mutationFn: () => api.linkSession(session.id, { workstream, ...(task === '' ? {} : { task }) }),
    onSuccess: onClose,
  });
  const queries = [projects, workstreams, tasks];
  const loading = queries.some((q) => q.isPending);
  const failed = queries.some((q) => q.error !== null);
  const choices = (tasks.data ?? []).filter((t) => t.workstream === workstream);
  const selectClass = 'w-full rounded-md border border-line bg-card px-3 py-2 text-sm text-ink';
  return (
    <Dialog open onOpenChange={(open) => { if (!open && !link.isPending) onClose(); }}>
      <DialogContent title="Link session" description="Choose a workstream and, optionally, a task.">
        <form onSubmit={(event) => { event.preventDefault(); link.mutate(); }}>
          <div className="flex flex-col gap-4 p-4">
            {loading && <p role="status">Loading work…</p>}
            {failed && <p role="alert">Could not load workstreams or tasks. Close and try again.</p>}
            <label htmlFor={`${id}-workstream`} className="flex flex-col gap-1 text-sm">
              Workstream
              <select id={`${id}-workstream`} className={selectClass} value={workstream} required disabled={loading || failed || link.isPending}
                onChange={(event) => { setWorkstream(event.target.value); setTask(''); link.reset(); }}>
                <option value="">Choose a workstream</option>
                {(workstreams.data ?? []).map((w) => (
                  <option key={w.id} value={w.id}>{projects.data?.find((p) => p.id === w.project)?.name} · {w.name}</option>
                ))}
              </select>
            </label>
            <label htmlFor={`${id}-task`} className="flex flex-col gap-1 text-sm">
              Task (optional)
              <select id={`${id}-task`} className={selectClass} value={task} disabled={workstream === '' || link.isPending || loading || failed}
                onChange={(event) => { setTask(event.target.value); link.reset(); }}>
                <option value="">No task</option>
                {choices.map((t) => <option key={t.id} value={t.id}>{t.key} · {t.title}</option>)}
              </select>
            </label>
            {link.error !== null && <p role="alert" className="text-sm text-risk">{link.error instanceof ApiError ? link.error.message : 'Could not link the session. Try again.'}</p>}
          </div>
          <DialogFooter>
            <Button type="button" variant="ghost" disabled={link.isPending} onClick={onClose}>Cancel</Button>
            <Button type="submit" disabled={workstream === '' || loading || failed || link.isPending}>{link.isPending ? 'Linking…' : 'Link session'}</Button>
          </DialogFooter>
        </form>
      </DialogContent>
    </Dialog>
  );
}
