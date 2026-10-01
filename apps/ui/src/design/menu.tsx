// Dropdown menus on Radix: arrow keys move, Enter or Space picks, Esc closes and returns focus to
// the trigger, and typing jumps to an item.

import { DropdownMenu } from 'radix-ui';
import type { ComponentProps, ReactNode } from 'react';
import { cx } from '../lib/cx.ts';
import { CheckIcon } from './icons.tsx';
import { Kbd } from './kbd.tsx';

export const Menu = DropdownMenu.Root;
export const MenuTrigger = DropdownMenu.Trigger;
export const MenuGroup = DropdownMenu.Group;
export const MenuRadioGroup = DropdownMenu.RadioGroup;

export function MenuContent({
  className,
  align = 'start',
  sideOffset = 4,
  ...props
}: ComponentProps<typeof DropdownMenu.Content>) {
  return (
    <DropdownMenu.Portal>
      <DropdownMenu.Content
        align={align}
        sideOffset={sideOffset}
        className={cx(
          'z-50 min-w-48 rounded-md border border-line bg-card p-1 text-sm text-ink shadow-pop',
          className,
        )}
        {...props}
      />
    </DropdownMenu.Portal>
  );
}

const ITEM =
  'flex h-7 cursor-default items-center gap-2 rounded-sm px-2 outline-none select-none ' +
  'data-[highlighted]:bg-hover data-[disabled]:pointer-events-none data-[disabled]:opacity-50';

export function MenuItem({
  className,
  icon,
  keys,
  children,
  ...props
}: ComponentProps<typeof DropdownMenu.Item> & { icon?: ReactNode; keys?: readonly string[] }) {
  return (
    <DropdownMenu.Item className={cx(ITEM, className)} {...props}>
      {icon !== undefined && <span className="text-ink-2">{icon}</span>}
      <span className="min-w-0 flex-1 truncate">{children}</span>
      {keys !== undefined && <Kbd keys={keys} />}
    </DropdownMenu.Item>
  );
}

export function MenuRadioItem({
  className,
  children,
  ...props
}: ComponentProps<typeof DropdownMenu.RadioItem>) {
  return (
    <DropdownMenu.RadioItem className={cx(ITEM, 'pl-7 relative', className)} {...props}>
      <DropdownMenu.ItemIndicator className="absolute left-2 inline-flex">
        <CheckIcon className="size-3.5" />
      </DropdownMenu.ItemIndicator>
      <span className="min-w-0 flex-1 truncate">{children}</span>
    </DropdownMenu.RadioItem>
  );
}

export function MenuLabel({ className, ...props }: ComponentProps<typeof DropdownMenu.Label>) {
  return (
    <DropdownMenu.Label
      className={cx('px-2 pt-1.5 pb-1 text-xs font-medium text-ink-2', className)}
      {...props}
    />
  );
}

export function MenuSeparator({ className, ...props }: ComponentProps<typeof DropdownMenu.Separator>) {
  return <DropdownMenu.Separator className={cx('-mx-1 my-1 h-px bg-line', className)} {...props} />;
}
