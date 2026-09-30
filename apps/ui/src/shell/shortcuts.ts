// The shell's global shortcuts, with Cmd in place of Ctrl on macOS:
// Ctrl . switches layout, Ctrl K opens the palette, Ctrl J the Orchestrator, Ctrl B the sidebar.

import type { AnyRouter } from '@tanstack/react-router';
import { useEffect } from 'react';
import { hasMod } from '../lib/platform.ts';
import { switchLayout } from './layout.ts';
import { useShell } from './store.ts';

export const SHORTCUTS = {
  layout: ['mod', '.'],
  palette: ['mod', 'k'],
  orchestrator: ['mod', 'j'],
  sidebar: ['mod', 'b'],
} as const;

export function useShellShortcuts(router: AnyRouter, ws: string): void {
  useEffect(() => {
    function onKeyDown(event: KeyboardEvent) {
      if (event.defaultPrevented || event.shiftKey || !hasMod(event)) return;
      const key = event.code === 'Period' ? '.' : event.key.toLowerCase();
      const shell = useShell.getState();
      const actions: Record<string, () => void> = {
        '.': () => switchLayout(router, ws),
        k: () => shell.setPaletteOpen(!shell.paletteOpen),
        j: () => shell.setOrchestratorOpen(!shell.orchestratorOpen),
        b: () => shell.setSidebarCollapsed(!shell.sidebarCollapsed),
      };
      const action = actions[key];
      if (action === undefined) return;
      event.preventDefault();
      // Holding the keys down must not flicker the panel.
      if (!event.repeat) action();
    }
    window.addEventListener('keydown', onKeyDown);
    return () => window.removeEventListener('keydown', onKeyDown);
  }, [router, ws]);
}
