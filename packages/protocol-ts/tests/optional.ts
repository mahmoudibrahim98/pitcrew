import type { NewTask, TaskPatch, TranscriptItem, EventBody } from '../index.ts';

const newTask: NewTask = { project: '01J00000000000000000000000', title: 'Synthetic task' };
const clear: TaskPatch = { due: null, workstream: null };
const unchanged: TaskPatch = {};
// @ts-expect-error Skipped fields are absent rather than null in serialized NewTask.
const nullTitle: NewTask = { ...newTask, description: null };
// @ts-expect-error exactOptionalPropertyTypes rejects a present undefined field.
const undefinedTitle: TaskPatch = { title: undefined };
// @ts-expect-error The protocol uses kind, not type, for transcript discriminants.
const wrongTag: TranscriptItem = { type: 'turn_ended', at: 0, offset: 0 };
// @ts-expect-error Event data remains adjacent to its type tag.
const flatEvent: EventBody = { type: 'session_ended', session: '01J00000000000000000000000' };
void [newTask, clear, unchanged, nullTitle, undefinedTitle, wrongTag, flatEvent];
