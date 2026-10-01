// Modal dialogs on Radix: focus is trapped inside, Esc closes, and focus returns to what opened it.

import { Dialog as RadixDialog, VisuallyHidden } from 'radix-ui';
import type { ComponentProps, ReactNode } from 'react';
import { cx } from '../lib/cx.ts';
import { CloseIcon } from './icons.tsx';

export const Dialog = RadixDialog.Root;
export const DialogTrigger = RadixDialog.Trigger;
export const DialogClose = RadixDialog.Close;

export function DialogContent({
  title,
  description,
  hideTitle = false,
  showClose = true,
  className,
  children,
  ...props
}: Omit<ComponentProps<typeof RadixDialog.Content>, 'title'> & {
  title: ReactNode;
  description?: ReactNode;
  /** Keep the title for screen readers only (the palette, which has its own input). */
  hideTitle?: boolean;
  showClose?: boolean;
}) {
  const heading = (
    <RadixDialog.Title className="text-md font-semibold text-ink">{title}</RadixDialog.Title>
  );
  return (
    <RadixDialog.Portal>
      <RadixDialog.Overlay className="fixed inset-0 z-40 bg-[rgb(10_10_14/0.45)]" />
      <RadixDialog.Content
        className={cx(
          'fixed top-[12vh] left-1/2 z-50 flex max-h-[76vh] w-[min(560px,calc(100vw-32px))] -translate-x-1/2 flex-col',
          'overflow-hidden rounded-lg border border-line bg-card text-ink shadow-pop outline-none',
          className,
        )}
        {...(description === undefined ? { 'aria-describedby': undefined } : {})}
        {...props}
      >
        {hideTitle ? (
          <VisuallyHidden.Root>{heading}</VisuallyHidden.Root>
        ) : (
          <div className="flex items-center gap-3 border-b border-line px-4 py-3">
            {heading}
            {showClose && (
              <RadixDialog.Close
                aria-label="Close"
                className="ml-auto inline-flex size-7 items-center justify-center rounded-sm text-ink-2 hover:bg-hover hover:text-ink"
              >
                <CloseIcon />
              </RadixDialog.Close>
            )}
          </div>
        )}
        {description !== undefined && (
          <RadixDialog.Description className={cx('px-4 text-sm text-ink-2', hideTitle ? 'sr-only' : 'pt-3')}>
            {description}
          </RadixDialog.Description>
        )}
        {children}
      </RadixDialog.Content>
    </RadixDialog.Portal>
  );
}

/** Buttons along the bottom of a dialog. */
export function DialogFooter({ className, ...props }: ComponentProps<'div'>) {
  return (
    <div
      className={cx('flex items-center justify-end gap-2 border-t border-line px-4 py-3', className)}
      {...props}
    />
  );
}
