import { Dialog } from 'radix-ui';
import { useRef, type ComponentProps, type ReactNode } from 'react';

export const SideDrawerTitle = Dialog.Title;
export const SideDrawerClose = Dialog.Close;
export function SideDrawer({ open, onOpenChange, children, onOpenAutoFocus, onCloseAutoFocus, ...props }: {
  open: boolean;
  onOpenChange(open: boolean): void;
  children: ReactNode;
} & Omit<ComponentProps<typeof Dialog.Content>, 'children'>) {
  const opener = useRef<HTMLElement | null>(null);
  return <Dialog.Root open={open} onOpenChange={onOpenChange}>
    <Dialog.Portal>
      <Dialog.Overlay className="fixed inset-0 z-40 bg-ink/20" />
      <Dialog.Content aria-describedby={undefined}
        onOpenAutoFocus={(event) => {
          opener.current = document.activeElement instanceof HTMLElement ? document.activeElement : null;
          onOpenAutoFocus?.(event);
        }}
        onCloseAutoFocus={(event) => {
          onCloseAutoFocus?.(event);
          if (!event.defaultPrevented && opener.current?.isConnected) { event.preventDefault(); opener.current.focus(); }
        }}
        className="fixed inset-y-0 right-0 z-50 flex w-full max-w-2xl flex-col overflow-y-auto border-l border-line bg-bg p-5 shadow-pop"
        {...props}>{children}</Dialog.Content>
    </Dialog.Portal>
  </Dialog.Root>;
}
