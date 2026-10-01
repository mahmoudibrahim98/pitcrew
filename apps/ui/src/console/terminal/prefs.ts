// The terminal's per-browser settings (`pitcrew.terminal` in localStorage): whether xterm's screen
// reader mode is on. About the person at this screen, so not per workspace or session.

import { create } from 'zustand';
import { persist } from 'zustand/middleware';

interface TerminalPrefs {
  screenReader: boolean;
  setScreenReader(on: boolean): void;
}

export const useTerminalPrefs = create<TerminalPrefs>()(
  persist((set) => ({ screenReader: false, setScreenReader: (screenReader) => set({ screenReader }) }), {
    name: 'pitcrew.terminal',
    version: 1,
    partialize: ({ screenReader }) => ({ screenReader }),
    merge: (stored, current) => {
      const saved = (stored ?? {}) as { screenReader?: unknown };
      return { ...current, screenReader: saved.screenReader === true };
    },
  }),
);
