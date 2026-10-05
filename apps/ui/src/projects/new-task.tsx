// "+ New" → "Task": replaces the shell's placeholder. Minimal fields; the hub assigns the key.

import { useId, useState, type FormEvent } from 'react';
import { Button, DialogFooter } from '../design/index.ts';
import { useCreateTask, useMembers, useProjects, useWorkstreams } from './data.ts';
import { useProjectsNav } from './nav.tsx';
import { ErrorNote, Field, inputClass } from './ui.tsx';
import type { Priority, TaskStatus } from '../data/index.ts';
import { PRIORITY, TASK_STATUS, STATUS_ORDER } from './format.ts';

export function NewTaskDialog({ close, defaults = {} }: { close(): void; defaults?: { project?: string; workstream?: string; status?: TaskStatus } }) {
  const titleId = useId();
  const projects = useProjects();
  const members = useMembers();
  const create = useCreateTask();
  const nav = useProjectsNav();
  const context = window.location.pathname.match(/\/projects\/([^/]+)(?:\/workstreams\/([^/]+))?/);
  const [project, setProject] = useState(defaults.project ?? context?.[1] ?? '');
  const [workstream, setWorkstream] = useState(defaults.workstream ?? context?.[2] ?? '');
  const [title, setTitle] = useState('');
  const [description, setDescription] = useState('');
  const [priority, setPriority] = useState<Priority>('none');
  const [labels, setLabels] = useState('');
  const [status, setStatus] = useState<TaskStatus>(defaults.status ?? 'todo');
  const [assignee, setAssignee] = useState('');
  const [due, setDue] = useState('');
  const projectId = project !== '' ? project : (projects.data?.[0]?.id ?? '');
  const workstreams = useWorkstreams(projectId || undefined);

  const submit = (e: FormEvent) => {
    e.preventDefault();
    const text = title.trim();
    const inProject = projectId;
    if (text === '' || inProject === '') return;
    create.mutate(
      {
        project: inProject,
        title: text,
        description, priority, status,
        labels: labels.split(/\r?\n/).map((label) => label.trim()).filter(Boolean),
        ...(workstream === '' ? {} : { workstream }),
        ...(assignee === '' ? {} : { assignee }),
        ...(due === '' ? {} : { due }),
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
            {(workstreams.data ?? []).filter((w) => w.project === projectId).map((w) => (
              <option key={w.id} value={w.id}>
                {w.name}
              </option>
            ))}
          </select>
        )}
      </Field>
      <Field label="Description">{(id) => <textarea id={id} rows={4} className={inputClass} value={description} onChange={(event) => setDescription(event.target.value)} />}</Field>
      <Field label="Priority">{(id) => <select id={id} className={inputClass} value={priority} onChange={(event) => setPriority(event.target.value as Priority)}>{Object.entries(PRIORITY).map(([value, item]) => <option key={value} value={value}>{item.label}</option>)}</select>}</Field>
      <Field label="Labels" hint="One label per line.">{(id) => <textarea id={id} rows={2} className={inputClass} value={labels} onChange={(event) => setLabels(event.target.value)} />}</Field>
      <Field label="Status">{(id) => <select id={id} className={inputClass} value={status} onChange={(event) => setStatus(event.target.value as TaskStatus)}>{STATUS_ORDER.map((value) => <option key={value} value={value}>{TASK_STATUS[value].label}</option>)}</select>}</Field>
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
      <Field label="Due date (optional)">
        {(id) => <input id={id} type="date" value={due} onChange={(event) => setDue(event.target.value)} className={inputClass} />}
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
