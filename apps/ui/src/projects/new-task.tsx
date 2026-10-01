// "+ New" → "Task": replaces the shell's placeholder. Minimal fields; the hub assigns the key.

import { useId, useState, type FormEvent } from 'react';
import { Button, DialogFooter } from '../design/index.ts';
import { useCreateTask, useMembers, useProjects, useWorkstreams } from './data.ts';
import { useProjectsNav } from './nav.tsx';
import { ErrorNote, Field, inputClass } from './ui.tsx';

export function NewTaskDialog({ close }: { close(): void }) {
  const titleId = useId();
  const projects = useProjects();
  const members = useMembers();
  const create = useCreateTask();
  const nav = useProjectsNav();
  const [project, setProject] = useState('');
  const [workstream, setWorkstream] = useState('');
  const [title, setTitle] = useState('');
  const [assignee, setAssignee] = useState('');
  const workstreams = useWorkstreams(project === '' ? undefined : project);
  const projectId = project !== '' ? project : (projects.data?.[0]?.id ?? '');

  const submit = (e: FormEvent) => {
    e.preventDefault();
    const text = title.trim();
    const inProject = projectId;
    if (text === '' || inProject === '') return;
    create.mutate(
      {
        project: inProject,
        title: text,
        ...(workstream === '' ? {} : { workstream }),
        ...(assignee === '' ? {} : { assignee }),
      },
      {
        onSuccess: (task) => {
          close();
          nav.openTask?.(task.id);
        },
      },
    );
  };

  return (
    <form onSubmit={submit} aria-labelledby={titleId} className="flex flex-col gap-3 px-4 py-4">
      <h2 id={titleId} className="sr-only">
        New task
      </h2>
      <Field label="Title">
        {(id) => (
          <input
            id={id}
            required
            autoFocus
            value={title}
            onChange={(e) => setTitle(e.target.value)}
            className={inputClass}
          />
        )}
      </Field>
      <Field label="Project">
        {(id) => (
          <select
            id={id}
            value={projectId}
            onChange={(e) => {
              setProject(e.target.value);
              setWorkstream('');
            }}
            className={inputClass}
          >
            {(projects.data ?? []).map((p) => (
              <option key={p.id} value={p.id}>
                {p.name}
              </option>
            ))}
          </select>
        )}
      </Field>
      <Field label="Workstream (optional)">
        {(id) => (
          <select id={id} value={workstream} onChange={(e) => setWorkstream(e.target.value)} className={inputClass}>
            <option value="">None</option>
            {(workstreams.data ?? []).map((w) => (
              <option key={w.id} value={w.id}>
                {w.name}
              </option>
            ))}
          </select>
        )}
      </Field>
      <Field label="Assignee (optional)">
        {(id) => (
          <select id={id} value={assignee} onChange={(e) => setAssignee(e.target.value)} className={inputClass}>
            <option value="">Unassigned</option>
            {(members.data ?? []).map((m) => (
              <option key={m.id} value={m.id}>
                {m.handle}
              </option>
            ))}
          </select>
        )}
      </Field>
      {create.error !== null && <ErrorNote error={create.error} what="create the task" />}
      <DialogFooter>
        <Button type="submit" variant="primary" disabled={create.isPending || title.trim() === '' || projectId === ''}>
          Create
        </Button>
      </DialogFooter>
    </form>
  );
}
