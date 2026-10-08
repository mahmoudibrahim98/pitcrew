import { useState, type FormEvent } from 'react';
import { Button } from '../design/index.ts';
import type { Task, Priority, TaskPatch } from '../data/index.ts';
import { usePatchTask, useWorkstreams } from './data.ts';
import { PRIORITY } from './format.ts';
import { ErrorNote, Field, inputClass } from './ui.tsx';

/** What the edit form holds, in the form's terms: an empty date or workstream is `null`. */
export interface TaskForm {
  title: string;
  description: string;
  priority: Priority;
  labels: string[];
  start: string | null;
  due: string | null;
  workstream: string | null;
  accept_auto: boolean;
}

const sameList = (a: readonly string[], b: readonly string[]) => a.length === b.length && a.every((x, i) => x === b[i]);

/**
 * Only the fields changed in the form since it opened (`opened`). Sending the rest would put back
 * what the form showed when it opened over a concurrent edit or sync of those fields, and the
 * write-back would then offer to push the old values upstream.
 */
export function changedFields(opened: Task, form: TaskForm): TaskPatch {
  const patch: TaskPatch = {};
  if (form.title !== opened.title) patch.title = form.title;
  if (form.description !== opened.description) patch.description = form.description;
  if (form.priority !== opened.priority) patch.priority = form.priority;
  if (!sameList(form.labels, opened.labels)) patch.labels = form.labels;
  if (form.start !== (opened.start ?? null)) patch.start = form.start;
  if (form.due !== (opened.due ?? null)) patch.due = form.due;
  if (form.workstream !== (opened.workstream ?? null)) patch.workstream = form.workstream;
  if (form.accept_auto !== opened.accept_auto) patch.accept_auto = form.accept_auto;
  return patch;
}

export function TaskEditor({ task, close }: { task: Task; close(): void }) {
  const save = usePatchTask();
  const workstreams = useWorkstreams(task.project);
  // The task as the form first showed it; the live `task` may change under the form meanwhile.
  const [opened] = useState(task);
  const [title, setTitle] = useState(task.title);
  const [description, setDescription] = useState(task.description);
  const [priority, setPriority] = useState(task.priority);
  const [labels, setLabels] = useState(task.labels.join('\n'));
  const [start, setStart] = useState(task.start ?? '');
  const [due, setDue] = useState(task.due ?? '');
  const [workstream, setWorkstream] = useState(task.workstream ?? '');
  const [acceptAuto, setAcceptAuto] = useState(task.accept_auto);
  function submit(event: FormEvent) {
    event.preventDefault();
    const patch = changedFields(opened, {
      title, description, priority,
      labels: labels.split(/\r?\n/).map((label) => label.trim()).filter(Boolean),
      start: start || null, due: due || null, workstream: workstream || null, accept_auto: acceptAuto,
    });
    if (Object.keys(patch).length === 0) {
      close();
      return;
    }
    save.mutate({ task: task.id, patch }, { onSuccess: close });
  }
  return <form aria-label="Edit task" onSubmit={submit} className="flex flex-col gap-3 rounded-md border border-line p-3">
    <Field label="Title">{(id) => <input id={id} autoFocus required maxLength={500} value={title} onChange={(event) => setTitle(event.target.value)} className={inputClass} />}</Field>
    <Field label="Description" hint="Markdown; raw HTML is displayed as text.">{(id) => <textarea id={id} value={description} onChange={(event) => setDescription(event.target.value)} rows={6} className={inputClass} />}</Field>
    <Field label="Priority">{(id) => <select id={id} value={priority} onChange={(event) => setPriority(event.target.value as Priority)} className={inputClass}>
      {Object.entries(PRIORITY).map(([value, { label }]) => <option key={value} value={value}>{label}</option>)}
    </select>}</Field>
    <Field label="Labels" hint="One label per line.">{(id) => <textarea id={id} rows={2} value={labels} onChange={(event) => setLabels(event.target.value)} className={inputClass} />}</Field>
    <Field label="Start date">{(id) => <input id={id} type="date" value={start} onChange={(event) => setStart(event.target.value)} className={inputClass} />}</Field>
    <Field label="Due date">{(id) => <input id={id} type="date" value={due} onChange={(event) => setDue(event.target.value)} className={inputClass} />}</Field>
    <Field label="Workstream">{(id) => <select id={id} value={workstream} onChange={(event) => setWorkstream(event.target.value)} className={inputClass}>
      <option value="">None</option>{workstreams.data?.map((item) => <option key={item.id} value={item.id}>{item.name}</option>)}
    </select>}</Field>
    <label className="flex gap-2 text-sm"><input type="checkbox" checked={acceptAuto} onChange={(event) => setAcceptAuto(event.target.checked)} />Allow automatic completion after review</label>
    {save.error !== null && <ErrorNote error={save.error} what="save the task; check the fields and try again" />}
    <div className="flex gap-2"><Button type="submit" variant="primary" disabled={save.isPending}>Save task</Button><Button onClick={close} disabled={save.isPending}>Cancel</Button></div>
  </form>;
}
