// A side panel with a draggable edge. The edge is a focusable separator (the ARIA window-splitter
// pattern): arrow keys resize, Home and End go to the minimum and maximum.

import { useId, useRef, type KeyboardEvent, type PointerEvent, type ReactNode } from 'react';
import { cx } from '../lib/cx.ts';

export function ResizablePanel({
  as: Element = 'aside',
  label,
  width,
  onWidthChange,
  min = 240,
  max = 720,
  step = 16,
  side = 'right',
  className,
  children,
}: {
  as?: 'aside' | 'section' | 'div';
  /** The panel's accessible name; the separator is "Resize <label>". */
  label: string;
  width: number;
  onWidthChange(width: number): void;
  min?: number;
  max?: number;
  step?: number;
  /** Which side of the window the panel sits on; its free edge faces the other way. */
  side?: 'left' | 'right';
  className?: string;
  children: ReactNode;
}) {
  const id = useId();
  const drag = useRef<{ x: number; width: number } | null>(null);
  const clamp = (value: number) => Math.round(Math.min(max, Math.max(min, value)));
  // Moving the pointer away from the panel's side grows it.
  const grow = side === 'right' ? -1 : 1;

  function onPointerDown(event: PointerEvent<HTMLDivElement>) {
    if (event.button !== 0) return;
    event.preventDefault();
    event.currentTarget.setPointerCapture(event.pointerId);
    drag.current = { x: event.clientX, width };
  }

  function onPointerMove(event: PointerEvent<HTMLDivElement>) {
    const start = drag.current;
    if (start === null) return;
    onWidthChange(clamp(start.width + grow * (event.clientX - start.x)));
  }

  function onPointerEnd(event: PointerEvent<HTMLDivElement>) {
    drag.current = null;
    if (event.currentTarget.hasPointerCapture(event.pointerId)) {
      event.currentTarget.releasePointerCapture(event.pointerId);
    }
  }

  function onKeyDown(event: KeyboardEvent<HTMLDivElement>) {
    // ArrowLeft moves the edge left: a right-hand panel grows, a left-hand one shrinks.
    const next: Record<string, number> = {
      ArrowLeft: width - grow * step,
      ArrowRight: width + grow * step,
      Home: min,
      End: max,
    };
    const value = next[event.key];
    if (value === undefined) return;
    event.preventDefault();
    onWidthChange(clamp(value));
  }

  return (
    <Element
      id={id}
      aria-label={label}
      style={{ width }}
      className={cx('relative flex shrink-0 flex-col', className)}
    >
      <div
        role="separator"
        aria-orientation="vertical"
        aria-label={`Resize ${label}`}
        aria-controls={id}
        aria-valuenow={width}
        aria-valuemin={min}
        aria-valuemax={max}
        tabIndex={0}
        onPointerDown={onPointerDown}
        onPointerMove={onPointerMove}
        onPointerUp={onPointerEnd}
        onPointerCancel={onPointerEnd}
        onKeyDown={onKeyDown}
        className={cx(
          'group absolute inset-y-0 z-10 w-2 cursor-col-resize touch-none outline-none',
          side === 'right' ? '-left-1' : '-right-1',
        )}
      >
        <span
          aria-hidden
          className="absolute inset-y-0 left-1/2 w-0.5 -translate-x-1/2 bg-transparent transition-colors group-hover:bg-line-2 group-focus-visible:bg-accent"
        />
      </div>
      {children}
    </Element>
  );
}
