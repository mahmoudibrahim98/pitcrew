// Small pieces the projects views share. Candidates for src/design once they settle (ask L).

import { useId, type ReactNode } from 'react';
import { cx } from '../lib/cx.ts';

export const inputClass =
  'w-full rounded-sm border border-line-2 bg-card px-2 py-1 text-sm text-ink placeholder:text-ink-2 focus-visible:outline-2';

/** A label and its control, associated by id. */
export function Field({
  label,
  hint,
  className,
  children,
}: {
  label: string;
  hint?: string;
  className?: string;
  children: (id: string) => ReactNode;
}) {
  const id = useId();
  return (
    <div className={cx('flex flex-col gap-1', className)}>
      <label htmlFor={id} className="text-xs font-medium text-ink-2">
        {label}
      </label>
      {children(id)}
      {hint !== undefined && <p className="text-xs text-ink-2">{hint}</p>}
    </div>
  );
}

/** A failed request, with the server's message. */
export function ErrorNote({ error, what }: { error: Error; what: string }) {
  return (
    <p role="alert" className="text-sm text-risk">
      Couldn’t {what}: {error.message}
    </p>
  );
}

/** Text for screen readers only. */
export function VisuallyHidden({ children, id }: { children: ReactNode; id?: string }) {
  return (
    <span id={id} className="sr-only">
      {children}
    </span>
  );
}

/** A titled block of a composite page. */
export function Panel({
  title,
  actions,
  className,
  children,
}: {
  title: string;
  actions?: ReactNode;
  className?: string;
  children: ReactNode;
}) {
  const id = useId();
  return (
    <section aria-labelledby={id} className={cx('rounded-md border border-line bg-card p-4', className)}>
      <header className="mb-3 flex flex-wrap items-center gap-2">
        <h2 id={id} className="text-lg font-semibold">
          {title}
        </h2>
        {actions !== undefined && <div className="ml-auto flex items-center gap-2">{actions}</div>}
      </header>
      {children}
    </section>
  );
}

/** A link-like button when there is somewhere to go, plain text when not. */
export function MaybeLink({
  onOpen,
  className,
  children,
}: {
  onOpen: (() => void) | undefined;
  className?: string;
  children: ReactNode;
}) {
  if (onOpen === undefined) return <span className={className}>{children}</span>;
  return (
    <button
      type="button"
      onClick={onOpen}
      className={cx('rounded-sm text-left underline-offset-2 hover:underline', className)}
    >
      {children}
    </button>
  );
}
