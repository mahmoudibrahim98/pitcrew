// A navigation tree (the ARIA "navigation treeview" pattern): one tab stop; Up and Down move,
// Right opens or enters an item, Left closes it or goes to its parent, Home and End jump, and
// typing a letter jumps to the next item starting with it. Items are usually links, passed as
// the child and rendered with role="treeitem"; Enter follows them.

import { Slot } from 'radix-ui';
import {
  createContext,
  use,
  useEffect,
  useId,
  useState,
  type Dispatch,
  type KeyboardEvent,
  type MouseEvent,
  type ReactElement,
  type ReactNode,
  type SetStateAction,
} from 'react';
import { cx } from '../lib/cx.ts';
import { FOCUS_RING } from './focus.ts';
import { ChevronRightIcon } from './icons.tsx';

interface TreeState {
  active: string | null;
  fallback: string | undefined;
  setActive: Dispatch<SetStateAction<string | null>>;
}

const TreeContext = createContext<TreeState | null>(null);

function items(root: Element): HTMLElement[] {
  return [...root.querySelectorAll<HTMLElement>('[role="treeitem"]')];
}

export function Tree({
  label,
  defaultValue,
  className,
  children,
}: {
  label: string;
  /** The item that holds the tab stop until another is focused (the current page, or the first). */
  defaultValue: string | undefined;
  className?: string;
  children: ReactNode;
}) {
  const [active, setActive] = useState<string | null>(null);

  function onKeyDown(event: KeyboardEvent<HTMLUListElement>) {
    const target = event.target as HTMLElement;
    if (target.getAttribute('role') !== 'treeitem') return;
    const list = items(event.currentTarget);
    const at = list.indexOf(target);
    let next: HTMLElement | undefined;
    if (event.key === 'ArrowDown') next = list[at + 1];
    else if (event.key === 'ArrowUp') next = list[at - 1];
    else if (event.key === 'Home') next = list[0];
    else if (event.key === 'End') next = list.at(-1);
    else if (
      event.key.length === 1 &&
      /\S/.test(event.key) &&
      !event.ctrlKey &&
      !event.metaKey &&
      !event.altKey
    ) {
      const letter = event.key.toLowerCase();
      const order = [...list.slice(at + 1), ...list.slice(0, at + 1)];
      next = order.find((el) => (el.textContent ?? '').trim().toLowerCase().startsWith(letter));
    } else {
      return;
    }
    event.preventDefault();
    next?.focus();
  }

  return (
    <TreeContext value={{ active, fallback: defaultValue, setActive }}>
      <ul role="tree" aria-label={label} className={cx('flex flex-col gap-px', className)} onKeyDown={onKeyDown}>
        {children}
      </ul>
    </TreeContext>
  );
}

export function TreeItem({
  value,
  level,
  expanded,
  onExpandedChange,
  current = false,
  groupLabel,
  items: childItems,
  className,
  children,
}: {
  /** Unique within the tree. */
  value: string;
  /** 1 for top-level items. */
  level: number;
  /** Undefined for a leaf. */
  expanded?: boolean | undefined;
  onExpandedChange?: (expanded: boolean) => void;
  /** The item for the page on screen (`aria-current="page"`). */
  current?: boolean;
  groupLabel?: string;
  /** Child `TreeItem`s, rendered while expanded. */
  items?: ReactNode;
  className?: string;
  /** The element that becomes the treeitem, usually a link. */
  children: ReactElement;
}) {
  const tree = use(TreeContext);
  if (tree === null) throw new Error('TreeItem must be inside a Tree');
  const { setActive } = tree;
  const groupId = useId();
  const expandable = expanded !== undefined;
  const tabStop = (tree.active ?? tree.fallback) === value;

  // A removed item gives the tab stop back.
  useEffect(() => () => setActive((a) => (a === value ? null : a)), [setActive, value]);

  function toggle(open: boolean, item: HTMLElement | null) {
    // Closing a group that holds focus moves focus to its item first.
    const group = document.getElementById(groupId);
    if (!open && group?.contains(document.activeElement)) item?.focus();
    onExpandedChange?.(open);
  }

  function onKeyDown(event: KeyboardEvent<HTMLElement>) {
    const item = event.currentTarget;
    if (event.key === 'ArrowRight' && expandable) {
      event.preventDefault();
      if (!expanded) toggle(true, item);
      else document.getElementById(groupId)?.querySelector<HTMLElement>('[role="treeitem"]')?.focus();
    } else if (event.key === 'ArrowLeft') {
      event.preventDefault();
      if (expandable && expanded) toggle(false, item);
      else (item.closest('[role="group"]')?.previousElementSibling as HTMLElement | null)?.focus();
    }
  }

  function onChevron(event: MouseEvent<HTMLSpanElement>) {
    // The chevron sits inside the link: toggle without following it.
    event.preventDefault();
    event.stopPropagation();
    toggle(!expanded, event.currentTarget.closest<HTMLElement>('[role="treeitem"]'));
  }

  return (
    <li role="none" className="flex flex-col gap-px">
      <Slot.Root
        role="treeitem"
        aria-level={level}
        aria-expanded={expandable ? expanded : undefined}
        aria-owns={expandable && expanded ? groupId : undefined}
        aria-current={current ? 'page' : undefined}
        tabIndex={tabStop ? 0 : -1}
        onFocus={() => setActive(value)}
        onKeyDown={onKeyDown}
        style={{ paddingLeft: 4 + (level - 1) * 14 }}
        className={cx(
          'flex h-7 min-w-0 items-center gap-1 rounded-sm pr-2 text-sm text-ink-2 outline-none select-none',
          'hover:bg-hover hover:text-ink',
          FOCUS_RING,
          'aria-[current=page]:bg-card aria-[current=page]:font-medium aria-[current=page]:text-ink aria-[current=page]:shadow-[0_0_0_1px_var(--pc-line)]',
          className,
        )}
      >
        {expandable ? (
          <span
            aria-hidden
            onClick={onChevron}
            className="inline-flex size-5 shrink-0 items-center justify-center rounded-sm text-ink-2 hover:bg-sunken hover:text-ink"
          >
            <ChevronRightIcon className={cx('size-3.5 transition-transform', expanded && 'rotate-90')} />
          </span>
        ) : (
          <span aria-hidden className="inline-block w-5 shrink-0" />
        )}
        <Slot.Slottable>{children}</Slot.Slottable>
      </Slot.Root>
      {expandable && expanded && (
        <ul role="group" id={groupId} aria-label={groupLabel} className="flex flex-col gap-px">
          {childItems}
        </ul>
      )}
    </li>
  );
}
