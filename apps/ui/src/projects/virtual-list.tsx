// A list that renders only what is on screen once it gets long. Short lists render plainly, so they
// behave the same everywhere (including tests without layout).

import { useVirtualizer } from '@tanstack/react-virtual';
import { useEffect, useRef, type ReactNode } from 'react';
import { cx } from '../lib/cx.ts';

/** Lists longer than this are virtualised. */
export const VIRTUALIZE_AFTER = 60;

interface ListProps<T> {
  items: readonly T[];
  itemKey: (item: T) => string;
  renderItem: (item: T) => ReactNode;
  estimateSize?: number;
  label?: string;
  className?: string;
  /** An item to bring on screen (rendered, and scrolled to), such as a card about to take focus. */
  reveal?: string;
}

export function CardList<T>(props: ListProps<T>) {
  const { items, itemKey, renderItem, label, className } = props;
  if (items.length <= VIRTUALIZE_AFTER) {
    return (
      <ul aria-label={label} className={cx('flex flex-col gap-2', className)}>
        {items.map((item) => (
          <li key={itemKey(item)}>{renderItem(item)}</li>
        ))}
      </ul>
    );
  }
  return <VirtualCardList {...props} />;
}

function VirtualCardList<T>({ items, itemKey, renderItem, estimateSize = 112, label, className, reveal }: ListProps<T>) {
  const scroller = useRef<HTMLDivElement>(null);
  // TanStack Virtual returns functions the React Compiler must not memoise, so the compiler skips
  // this component; that is intended, and nothing here is passed to memoised children.
  // eslint-disable-next-line react-hooks/incompatible-library
  const virtualizer = useVirtualizer({
    count: items.length,
    getScrollElement: () => scroller.current,
    estimateSize: () => estimateSize,
    getItemKey: (index) => {
      const item = items[index];
      return item === undefined ? index : itemKey(item);
    },
    overscan: 6,
    gap: 8,
  });

  const revealAt = reveal === undefined ? -1 : items.findIndex((item) => itemKey(item) === reveal);
  useEffect(() => {
    if (revealAt >= 0) virtualizer.scrollToIndex(revealAt, { align: 'auto' });
  }, [revealAt, virtualizer]);

  return (
    <div ref={scroller} className={cx('max-h-[70vh] overflow-y-auto', className)}>
      <ul aria-label={label} className="relative" style={{ height: virtualizer.getTotalSize() }}>
        {virtualizer.getVirtualItems().map((row) => {
          const item = items[row.index];
          if (item === undefined) return null;
          return (
            <li
              key={row.key}
              data-index={row.index}
              ref={virtualizer.measureElement}
              aria-setsize={items.length}
              aria-posinset={row.index + 1}
              className="absolute top-0 left-0 w-full"
              style={{ transform: `translateY(${row.start}px)` }}
            >
              {renderItem(item)}
            </li>
          );
        })}
      </ul>
    </div>
  );
}
