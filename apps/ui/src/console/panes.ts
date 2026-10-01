// The console's pane sizes and whether the filters pane is open, remembered in this browser
// (`pitcrew.console` in localStorage). Per viewer, not per workspace: they are about the screen.

import { create } from 'zustand';
import { persist } from 'zustand/middleware';

export const PANE_WIDTH = {
  filters: { min: 180, max: 360, initial: 220 },
  list: { min: 260, max: 560, initial: 340 },
} as const;

/** Below this width (of the console, not the window) one pane shows at a time. */
export const NARROW_BELOW = 720;

interface Panes {
  filtersOpen: boolean;
  filtersWidth: number;
  listWidth: number;
}

interface PaneActions {
  setFiltersOpen(open: boolean): void;
  setFiltersWidth(width: number): void;
  setListWidth(width: number): void;
}

export const initialPanes: Panes = {
  filtersOpen: true,
  filtersWidth: PANE_WIDTH.filters.initial,
  listWidth: PANE_WIDTH.list.initial,
};

const clamp = (value: number, { min, max }: { min: number; max: number }) =>
  Number.isFinite(value) ? Math.round(Math.min(max, Math.max(min, value))) : min;

export const usePanes = create<Panes & PaneActions>()(
  persist(
    (set) => ({
      ...initialPanes,
      setFiltersOpen: (filtersOpen) => set({ filtersOpen }),
      setFiltersWidth: (width) => set({ filtersWidth: clamp(width, PANE_WIDTH.filters) }),
      setListWidth: (width) => set({ listWidth: clamp(width, PANE_WIDTH.list) }),
    }),
    {
      name: 'pitcrew.console',
      version: 1,
      partialize: ({ filtersOpen, filtersWidth, listWidth }): Panes => ({ filtersOpen, filtersWidth, listWidth }),
      // A stored value from an older build, or edited by hand, is brought back into range.
      merge: (stored, current) => {
        const saved = (stored ?? {}) as Partial<Panes>;
        return {
          ...current,
          filtersOpen: typeof saved.filtersOpen === 'boolean' ? saved.filtersOpen : current.filtersOpen,
          filtersWidth: clamp(saved.filtersWidth ?? current.filtersWidth, PANE_WIDTH.filters),
          listWidth: clamp(saved.listWidth ?? current.listWidth, PANE_WIDTH.list),
        };
      },
    },
  ),
);
