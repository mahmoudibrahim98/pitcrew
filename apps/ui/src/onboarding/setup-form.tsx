// The setup form (`POST /v1/setup`): the workspace's name, your name and handle (suggested from
// your name until you edit it), and the machine's name. Checked as the contract checks it before
// anything is sent; the hub's `400` and `409` land by the right field too, and a `409` because the
// workspace was set up meanwhile goes to `onAlreadySetUp` (Home). Used by the first-run wizard's
// workspace step and by the connect wizard, for a fresh remote hub.

import { useId, useRef, useState, type ReactNode } from 'react';
import { cx } from '../lib/cx.ts';
import { SetupRefused, type SetupWorkspaceInput, type SetupWorkspaceResult } from './api.ts';
import {
  checkSetup,
  LIMITS,
  suggestHandle,
  trimmedSetup,
  type SetupErrors,
  type SetupField,
  type SetupValues,
} from './validation.ts';

const FIELDS: readonly SetupField[] = ['workspaceName', 'personName', 'handle', 'machineName'];

export function toSetupInput(values: SetupValues): SetupWorkspaceInput {
  const trimmed = trimmedSetup(values);
  return {
    workspaceName: trimmed.workspaceName,
    person: { name: trimmed.personName, handle: trimmed.handle },
    machineName: trimmed.machineName,
  };
}

export function SetupForm({
  values,
  handleEdited,
  onChange,
  submit,
  onDone,
  onAlreadySetUp,
  machineLabel = 'This machine’s name',
  footer,
}: {
  values: SetupValues;
  /** The person typed a handle: stop suggesting one from the name. */
  handleEdited: boolean;
  onChange(values: SetupValues, handleEdited: boolean): void;
  submit(input: SetupWorkspaceInput): Promise<SetupWorkspaceResult>;
  onDone(result: SetupWorkspaceResult): void;
  /** Someone else set the workspace up first: nothing is left to do here. */
  onAlreadySetUp(): void;
  machineLabel?: string;
  /** The buttons, inside the form (its submit button sends it). */
  footer(busy: boolean): ReactNode;
}) {
  const [errors, setErrors] = useState<SetupErrors>({});
  const [formError, setFormError] = useState<string | null>(null);
  const [busy, setBusy] = useState(false);
  const inputs = useRef<Partial<Record<SetupField, HTMLInputElement | null>>>({});

  function change(field: SetupField, value: string) {
    let edited = handleEdited;
    let next: SetupValues = { ...values, [field]: value };
    if (field === 'handle') edited = value !== '';
    // The handle follows the name until the person types one (clearing it resumes that).
    if (field === 'personName' && !edited) next = { ...next, handle: suggestHandle(value) };
    if (errors[field] !== undefined) setErrors((e) => ({ ...e, [field]: undefined }));
    onChange(next, edited);
  }

  function show(found: SetupErrors) {
    setErrors(found);
    const first = FIELDS.find((f) => found[f] !== undefined);
    if (first !== undefined) inputs.current[first]?.focus();
  }

  async function send() {
    setFormError(null);
    const found = checkSetup(values);
    if (Object.keys(found).length > 0) {
      show(found);
      return;
    }
    setBusy(true);
    let result: SetupWorkspaceResult;
    try {
      result = await submit(toSetupInput(values));
    } catch (cause) {
      setBusy(false);
      if (cause instanceof SetupRefused && cause.alreadySetUp) {
        onAlreadySetUp();
      } else if (cause instanceof SetupRefused && cause.field !== undefined) {
        show({ [cause.field]: cause.message });
      } else {
        setFormError(cause instanceof Error ? cause.message : 'Could not set the workspace up.');
      }
      return;
    }
    setBusy(false);
    onDone(result);
  }

  const field = (name: SetupField, label: string, props: { hint?: string; placeholder?: string; max?: number }) => (
    <Field
      key={name}
      label={label}
      value={values[name]}
      error={errors[name]}
      hint={props.hint}
      placeholder={props.placeholder}
      max={props.max}
      inputRef={(element) => {
        inputs.current[name] = element;
      }}
      onChange={(value) => change(name, value)}
    />
  );

  return (
    <form
      noValidate
      onSubmit={(event) => {
        event.preventDefault();
        if (!busy) void send();
      }}
    >
      <div className="flex flex-col gap-4">
        {field('workspaceName', 'Workspace name', { placeholder: 'Demo Lab', max: LIMITS.workspaceName })}
        {field('personName', 'Your name', { placeholder: 'Sam Rivera', max: LIMITS.personName })}
        {field('handle', 'Your handle', {
          hint: 'How people and agents mention you: "@" and lower-case letters, digits, "_" or "-".',
          placeholder: '@sam',
        })}
        {field('machineName', machineLabel, { max: LIMITS.machineName })}
      </div>
      {formError !== null && (
        <p role="alert" className="mt-3 text-sm text-risk">
          {formError}
        </p>
      )}
      {footer(busy)}
    </form>
  );
}

function Field({
  label,
  value,
  error,
  hint,
  placeholder,
  max,
  inputRef,
  onChange,
}: {
  label: string;
  value: string;
  error: string | undefined;
  hint: string | undefined;
  placeholder: string | undefined;
  max: number | undefined;
  inputRef(element: HTMLInputElement | null): void;
  onChange(value: string): void;
}) {
  const id = useId();
  const described = [hint === undefined ? undefined : `${id}-hint`, error === undefined ? undefined : `${id}-error`]
    .filter((x) => x !== undefined)
    .join(' ');
  return (
    <div className="flex flex-col gap-1.5">
      <label htmlFor={id} className="text-sm font-medium text-ink">
        {label}
      </label>
      <input
        id={id}
        ref={inputRef}
        type="text"
        autoComplete="off"
        spellCheck={false}
        value={value}
        placeholder={placeholder}
        aria-invalid={error !== undefined}
        aria-describedby={described === '' ? undefined : described}
        // Room for a few over the limit, so a paste is seen and the message explains it.
        maxLength={max === undefined ? 64 : max * 2}
        onChange={(event) => onChange(event.target.value)}
        className={cx(
          'h-8 rounded-sm border bg-card px-2.5 text-sm text-ink outline-none focus-visible:border-accent',
          error === undefined ? 'border-line-2' : 'border-risk',
        )}
      />
      {hint !== undefined && (
        <p id={`${id}-hint`} className="text-xs text-ink-2">
          {hint}
        </p>
      )}
      {error !== undefined && (
        <p id={`${id}-error`} role="alert" className="text-xs text-risk">
          {error}
        </p>
      )}
    </div>
  );
}
