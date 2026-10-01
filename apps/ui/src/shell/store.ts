// UI state for the shell. The layout and where each layout was last left persist per workspace;
// the sidebar, the expanded projects and the Orchestrator panel persist globally.

import { create } from 'zustand';
import { persist } from 'zustand/middleware';
import type { LayoutId } from './feature.ts';

export interface WorkspacePrefs {
  layout: LayoutId;
  /** The last page seen in each layout, as a path with its search. */
  last: Partial<Record<LayoutId, string>>;
}

export const ORCHESTRATOR_WIDTH = { min: 280, max: 640, initial: 360 } as const;

interface Persisted {
  workspaces: Record<string, WorkspacePrefs>;
  sidebarCollapsed: boolean;
  /** Projects open in the sidebar tree, by id. */
  expanded: Record<string, boolean>;
  orchestratorOpen: boolean;
  orchestratorWidth: number;
}

interface Transient {
  paletteOpen: boolean;
  /** The "+ New" dialog on screen, by entry id. */
  creating: string | null;
  /** What had focus before that dialog opened, to give focus back to. */
  creatingFrom: Element | null;
}

interface Actions {
  setLayout(ws: string, layout: LayoutId): void;
  remember(ws: string, layout: LayoutId, path: string): void;
  setSidebarCollapsed(collapsed: boolean): void;
  setExpanded(id: string, open: boolean): void;
  setOrchestratorOpen(open: boolean): void;
  setOrchestratorWidth(width: number): void;
  setPaletteOpen(open: boolean): void;
  /** Opens a "+ New" dialog, or closes it with null. `from` gets focus back when it closes. */
  setCreating(id: string | null, from?: Element | null): void;
}

export type ShellState = Persisted & Transient & Actions;

export const initialShellState: Persisted & Transient = {
  workspaces: {},
  sidebarCollapsed: false,
  expanded: {},
  orchestratorOpen: false,
  orchestratorWidth: ORCHESTRATOR_WIDTH.initial,
  paletteOpen: false,
  creating: null,
  creatingFrom: null,
};

function prefs(state: Persisted, ws: string): WorkspacePrefs {
  return state.workspaces[ws] ?? { layout: 'projects', last: {} };
}

export const useShell = create<ShellState>()(
  persist(
    (set) => ({
      ...initialShellState,
      setLayout: (ws, layout) =>
        set((s) => ({ workspaces: { ...s.workspaces, [ws]: { ...prefs(s, ws), layout } } })),
      remember: (ws, layout, path) =>
        set((s) => {
          const current = prefs(s, ws);
          if (current.layout === layout && current.last[layout] === path) return s;
          return {
            workspaces: { ...s.workspaces, [ws]: { layout, last: { ...current.last, [layout]: path } } },
          };
        }),
      setSidebarCollapsed: (sidebarCollapsed) => set({ sidebarCollapsed }),
      setExpanded: (id, open) => set((s) => ({ expanded: { ...s.expanded, [id]: open } })),
      setOrchestratorOpen: (orchestratorOpen) => set({ orchestratorOpen }),
      setOrchestratorWidth: (orchestratorWidth) => set({ orchestratorWidth }),
      setPaletteOpen: (paletteOpen) => set({ paletteOpen }),
      setCreating: (creating, from = null) =>
        // Closing keeps `creatingFrom`: the dialog reads it as it hands focus back.
        set((s) => ({ creating, creatingFrom: creating === null ? s.creatingFrom : from })),
    }),
    {
      name: 'pitcrew.shell',
      version: 1,
      partialize: (s): Persisted => ({
        workspaces: s.workspaces,
        sidebarCollapsed: s.sidebarCollapsed,
        expanded: s.expanded,
        orchestratorOpen: s.orchestratorOpen,
        orchestratorWidth: s.orchestratorWidth,
      }),
    },
  ),
);

/** The workspace's persisted layout (Projects until chosen otherwise). */
export function storedLayout(ws: string): LayoutId {
  return prefs(useShell.getState(), ws).layout;
}

export function lastPath(ws: string, layout: LayoutId): string | undefined {
  return prefs(useShell.getState(), ws).last[layout];
}
