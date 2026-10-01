// A non-modal popover for evidence and details: hovering or focusing the trigger previews its
// content; activating it (click, Enter or Space) opens it with focus moved in; Escape returns
// focus to the trigger, without previewing it again. Built from the popover `projects/recap-text.tsx`
// built for recap evidence (stream N will switch to this one once it lands).
//
// Positioned with Radix's `Popover`. Renders inline by default, as a direct DOM child (so it reads
// in document order right after its trigger, and the usual case needs no portal); pass `portal` to
// render it in one instead, for a trigger inside a container that would clip or stack under it.

import { Popover as RadixPopover } from 'radix-ui';
import { useEffect, useId, useRef, useState, type KeyboardEvent, type PointerEvent, type ReactNode } from 'react';
import { cx } from '../lib/cx.ts';
import { FOCUS_RING } from './focus.ts';

/**
 * `hover`: previewed under the pointer, which may move in to follow something in the content.
 * `focus`: previewed while the trigger has keyboard focus, inert so Tab moves on past it.
 * `open`: activated, focus inside the content.
 */
export type PopoverMode = 'closed' | 'hover' | 'focus' | 'open';

const HOVER_OPEN_MS = 300;
const HOVER_CLOSE_MS = 200;

const TRIGGER = cx('cursor-pointer rounded-sm outline-none', FOCUS_RING);

export interface PopoverProps {
  /** The trigger's content (often text); the trigger itself is a `span role="button"`. */
  children: ReactNode;
  /** The trigger's accessible name. */
  label: string;
  /** Shown while previewing (hover or focus) and once opened. */
  content: ReactNode;
  /** An accessible name for the content region. */
  contentLabel: string;
  /** Renders `content` in a portal instead of inline (the default: see the file comment). */
  portal?: boolean;
  side?: 'top' | 'right' | 'bottom' | 'left';
  align?: 'start' | 'center' | 'end';
  sideOffset?: number;
  className?: string;
  contentClassName?: string;
}

/** `mode`'s own callbacks: hooked up to the trigger and the content in `Popover`. */
function usePopoverMode() {
  const [mode, setMode] = useState<PopoverMode>('closed');
  const anchor = useRef<HTMLSpanElement>(null);
  const content = useRef<HTMLDivElement>(null);
  const timer = useRef<ReturnType<typeof setTimeout> | undefined>(undefined);
  // Set while focus goes back to the trigger on Escape, so that focus does not preview it again.
  const restoring = useRef(false);

  const clearTimer = () => {
    if (timer.current !== undefined) clearTimeout(timer.current);
    timer.current = undefined;
  };
  useEffect(() => {
    const pending = timer;
    return () => clearTimeout(pending.current);
  }, []);

  // Activated: focus goes into the content, where Tab moves between whatever it holds. This
  // covers a preview being activated; `onOpenAutoFocus` below covers content opened straight away
  // (it mounts a render later than `mode` changes).
  useEffect(() => {
    if (mode === 'open') content.current?.focus();
  }, [mode]);

  const preview = (by: 'hover' | 'focus') => {
    clearTimer();
    // Keyboard focus wins over the pointer; neither changes content already open.
    setMode((m) => (m === 'closed' || (m === 'hover' && by === 'focus') ? by : m));
  };
  const toggle = () => {
    clearTimer();
    setMode((m) => (m === 'open' ? 'closed' : 'open'));
  };
  const closeSoon = () => {
    clearTimer();
    timer.current = setTimeout(() => setMode((m) => (m === 'hover' ? 'closed' : m)), HOVER_CLOSE_MS);
  };
  const onPointerEnter = (e: PointerEvent) => {
    if (e.pointerType === 'touch') return;
    clearTimer();
    if (mode === 'closed') timer.current = setTimeout(() => preview('hover'), HOVER_OPEN_MS);
  };
  const onPointerLeave = (e: PointerEvent) => {
    if (e.pointerType !== 'touch') closeSoon();
  };
  const onKeyDown = (e: KeyboardEvent) => {
    if (e.key === 'Enter' || e.key === ' ') {
      e.preventDefault();
      toggle();
    }
  };
  /** Escape, while open: closes and returns focus to the trigger, without previewing it again. */
  const restoreFocus = () => {
    if (mode !== 'open') return;
    restoring.current = true;
    anchor.current?.focus();
    restoring.current = false;
  };
  /** The trigger gained focus: previews it, unless that focus is `restoreFocus()` giving it back. */
  const onTriggerFocus = () => {
    if (!restoring.current) preview('focus');
  };

  return { mode, setMode, anchor, content, clearTimer, toggle, onPointerEnter, onPointerLeave, onKeyDown, restoreFocus, onTriggerFocus };
}

export function Popover({
  children,
  label,
  content,
  contentLabel,
  portal = false,
  side = 'bottom',
  align = 'start',
  sideOffset = 6,
  className,
  contentClassName,
}: PopoverProps) {
  const {
    mode,
    setMode,
    anchor,
    content: contentRef,
    clearTimer,
    toggle,
    onPointerEnter,
    onPointerLeave,
    onKeyDown,
    restoreFocus,
    onTriggerFocus,
  } = usePopoverMode();
  const id = useId();
  const popoverContent = (
    <RadixPopover.Content
      ref={contentRef}
      id={id}
      aria-label={contentLabel}
      side={side}
      align={align}
      sideOffset={sideOffset}
      collisionPadding={12}
      inert={mode === 'focus'}
      onOpenAutoFocus={(e) => {
        e.preventDefault();
        if (mode === 'open') contentRef.current?.focus();
      }}
      onCloseAutoFocus={(e) => e.preventDefault()}
      onEscapeKeyDown={restoreFocus}
      onInteractOutside={(e) => {
        // The trigger itself is not "outside": its own click toggles it.
        if (e.target instanceof Node && anchor.current?.contains(e.target)) e.preventDefault();
      }}
      onPointerEnter={clearTimer}
      onPointerLeave={onPointerLeave}
      className={cx(
        'z-50 flex w-80 max-w-[calc(100vw-2rem)] flex-col gap-2 rounded-md border border-line bg-card p-3 text-sm text-ink shadow-pop outline-none',
        contentClassName,
      )}
    >
      {content}
    </RadixPopover.Content>
  );

  return (
    <RadixPopover.Root open={mode !== 'closed'} onOpenChange={(open) => !open && setMode('closed')}>
      <RadixPopover.Anchor asChild>
        <span
          ref={anchor}
          role="button"
          tabIndex={0}
          aria-label={label}
          aria-haspopup="dialog"
          aria-expanded={mode === 'open'}
          aria-controls={mode === 'open' ? id : undefined}
          className={cx(TRIGGER, className)}
          onClick={toggle}
          onKeyDown={onKeyDown}
          onFocus={onTriggerFocus}
          onBlur={(e) => {
            if (mode === 'focus' && !(contentRef.current?.contains(e.relatedTarget as Node | null) ?? false)) {
              clearTimer();
              setMode('closed');
            }
          }}
          onPointerEnter={onPointerEnter}
          onPointerLeave={onPointerLeave}
        >
          {children}
        </span>
      </RadixPopover.Anchor>
      {portal ? <RadixPopover.Portal>{popoverContent}</RadixPopover.Portal> : popoverContent}
    </RadixPopover.Root>
  );
}
