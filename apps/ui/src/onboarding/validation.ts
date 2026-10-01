// The checks the forms make before anything is sent, mirroring the contracts so a person sees the
// problem by the field, not as a refusal from the hub (which still checks, and whose `400` and
// `409` the forms show by the right field too):
// - setup (api-v1.md, "The first run"): names trimmed and counted in code points, the handle's
//   shape, no control characters;
// - a typed SSH host (desktop-gateway.md, "Remote workspaces"): never something ssh would read as
//   an option, so no leading `-`, and no whitespace or control characters;
// - SLURM job options: plain values, and a whole number of CPUs.

/** The setup form's fields, as the person sees them. */
export interface SetupValues {
  workspaceName: string;
  personName: string;
  handle: string;
  machineName: string;
}

export type SetupField = keyof SetupValues;

export type SetupErrors = Partial<Record<SetupField, string>>;

/** Each name's limit in code points, after trimming. */
export const LIMITS = { workspaceName: 80, personName: 80, machineName: 60 } as const;

// C0 and C1 control characters, as the hub refuses them.
// eslint-disable-next-line no-control-regex
const CONTROL = /[\u0000-\u001F\u007F-\u009F]/;
const HANDLE = /^@[a-z0-9_-]{1,32}$/;

/** Length in Unicode code points, as the hub counts it (not UTF-16 units). */
export function codePoints(text: string): number {
  return [...text].length;
}

/** `empty` says what to do when it is blank; `label` names the field in the other messages. */
function checkName(value: string, max: number, empty: string, label: string): string | undefined {
  const trimmed = value.trim();
  if (trimmed === '') return empty;
  if (CONTROL.test(trimmed)) return `${capitalize(label)} cannot contain control characters.`;
  if (codePoints(trimmed) > max) return `Keep ${label} to ${max} characters.`;
  return undefined;
}

function capitalize(text: string): string {
  return text.charAt(0).toUpperCase() + text.slice(1);
}

export function checkHandle(handle: string): string | undefined {
  if (handle === '') return 'Give yourself a handle.';
  if (!HANDLE.test(handle)) return 'A handle is "@" and 1 to 32 lower-case letters, digits, "_" or "-".';
  return undefined;
}

/** Every field's problem, if any; empty when the values can be sent. */
export function checkSetup(values: SetupValues): SetupErrors {
  const errors: SetupErrors = {};
  const workspace = checkName(values.workspaceName, LIMITS.workspaceName, 'Give the workspace a name.', 'the workspace name');
  const person = checkName(values.personName, LIMITS.personName, 'Enter your name.', 'your name');
  const handle = checkHandle(values.handle);
  const machine = checkName(values.machineName, LIMITS.machineName, 'Give this machine a name.', 'the machine name');
  if (workspace !== undefined) errors.workspaceName = workspace;
  if (person !== undefined) errors.personName = person;
  if (handle !== undefined) errors.handle = handle;
  if (machine !== undefined) errors.machineName = machine;
  return errors;
}

/** The values as they are sent: the names trimmed, the handle as typed (the hub does not trim it). */
export function trimmedSetup(values: SetupValues): SetupValues {
  return {
    workspaceName: values.workspaceName.trim(),
    personName: values.personName.trim(),
    handle: values.handle,
    machineName: values.machineName.trim(),
  };
}

/**
 * A handle from a name: the first word, folded to lower-case ASCII, with what a handle cannot hold
 * left out (`Sam Rivera` → `@sam`, `Zoë Ödegaard` → `@zoe`). Empty when nothing is left.
 */
export function suggestHandle(name: string): string {
  const first = name.trim().split(/\s+/)[0] ?? '';
  const folded = first
    .normalize('NFKD')
    .replace(/\p{M}/gu, '')
    .toLowerCase()
    .replace(/[^a-z0-9_-]/g, '')
    .slice(0, 32);
  return folded === '' ? '' : `@${folded}`;
}

/**
 * Which field a hub's `400` is about, from the field it names (`person.handle must be …`); none
 * when it names none of them, and the form shows it above its buttons instead.
 */
export function fieldOfMessage(message: string): SetupField | undefined {
  if (/\bworkspace_name\b/.test(message)) return 'workspaceName';
  if (/\bperson\.handle\b|\bhandle\b/.test(message)) return 'handle';
  if (/\bperson\.name\b/.test(message)) return 'personName';
  if (/\bmachine_name\b/.test(message)) return 'machineName';
  return undefined;
}

// eslint-disable-next-line no-control-regex
const WHITESPACE_OR_CONTROL = /[\s\u0000-\u001F\u007F-\u009F]/u;

/** A host the person typed (or picked): ssh must never read it as an option. */
export function checkHost(host: string): string | undefined {
  if (host === '') return 'Pick a host, or type one.';
  if (host.startsWith('-')) return 'A host cannot start with "-".';
  if (WHITESPACE_OR_CONTROL.test(host)) return 'A host cannot contain spaces or control characters.';
  if (codePoints(host) > 255) return 'That host name is too long.';
  return undefined;
}

/** A job option's text: one line of plain text, when given. */
export function checkJobText(value: string, what: string): string | undefined {
  if (CONTROL.test(value)) return `${capitalize(what)} cannot contain control characters.`;
  if (codePoints(value) > 200) return `Keep ${what} to 200 characters.`;
  return undefined;
}

/** CPUs, when given: a whole number from 1. */
export function checkCpus(value: string): string | undefined {
  if (value.trim() === '') return undefined;
  if (!/^\d+$/.test(value.trim()) || Number(value.trim()) < 1) return 'CPUs is a whole number, 1 or more.';
  return undefined;
}
