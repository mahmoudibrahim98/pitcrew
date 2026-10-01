import type { ReactNode } from 'react';

/** The page frame of the shell's own pages; its title reads "Loading…" while data arrives. */
export function Page({
  title,
  eyebrow,
  placeholder = true,
  children,
}: {
  title: string;
  eyebrow?: string | undefined;
  /** Says the page is a stand-in until its feature serves it. */
  placeholder?: boolean;
  children?: ReactNode;
}) {
  return (
    <div className="mx-auto flex max-w-4xl flex-col gap-4 px-6 py-6">
      <header>
        {eyebrow !== undefined && <p className="font-mono text-xs text-ink-2">{eyebrow}</p>}
        <h1 className="text-xl font-semibold">{title}</h1>
        {placeholder && <p className="mt-1 text-sm text-ink-2">A placeholder until its feature fills this page.</p>}
      </header>
      {children}
    </div>
  );
}

export const LOADING = 'Loading…';
