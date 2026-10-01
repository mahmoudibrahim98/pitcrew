// The terminal's keys outside the program.
//
// - View mode: Enter takes control; the arrows, Page Up/Down, Home and End scroll the scrollback;
//   Ctrl+C (Cmd+C on macOS) copies a selection. Every other key, the shell's included, is left
//   alone.
// - Control mode: every key goes to the program (Esc, Tab, F6, Ctrl K, J and B included) except
//   the release chord, Ctrl+Shift+X.
//
// Why Ctrl+Shift+X: xterm.js sends nothing at all for Ctrl+Shift with a letter (a terminal cannot
// tell it from Ctrl+letter, so it drops it), so no program in the terminal can be waiting for it;
// shells, editors, tmux, screen and the agent CLIs bind Ctrl+letter, Alt+letter, Esc sequences and
// F-keys instead. No browser binds Ctrl+Shift+X, the X reads as "leave", and it needs no F-key
// (laptops hide those behind Fn). Ctrl, not Cmd, on every platform, as terminal keys are.

import { isMac } from '../../lib/platform.ts';

/** For `Kbd` and `aria-keyshortcuts`. */
export const RELEASE_KEYS = ['Ctrl', 'Shift', 'X'] as const;
export const RELEASE_SHORTCUT = 'Control+Shift+X';
export const RELEASE_LABEL = 'Ctrl+Shift+X';

interface KeyLike {
  key: string;
  code: string;
  ctrlKey: boolean;
  shiftKey: boolean;
  altKey: boolean;
  metaKey: boolean;
}

/** Ctrl+Shift+X, by the key's character, or by its position on layouts without a Latin X. */
export function isReleaseChord(event: KeyLike): boolean {
  if (!event.ctrlKey || !event.shiftKey || event.altKey || event.metaKey) return false;
  const key = event.key.toLowerCase();
  return key === 'x' || (!/^[a-z]$/.test(key) && event.code === 'KeyX');
}

export type ViewKey =
  | { kind: 'control' }
  | { kind: 'scroll'; lines: number }
  | { kind: 'page'; pages: number }
  | { kind: 'top' }
  | { kind: 'bottom' }
  | { kind: 'copy' };

const plain = (event: KeyLike) => !event.ctrlKey && !event.metaKey && !event.altKey && !event.shiftKey;

/** What a key does in view mode, or undefined for a key the terminal leaves to others. */
export function viewModeKey(event: KeyLike): ViewKey | undefined {
  const copyModifier = isMac ? event.metaKey && !event.ctrlKey : event.ctrlKey && !event.metaKey;
  if (copyModifier && !event.altKey && !event.shiftKey && event.key.toLowerCase() === 'c') return { kind: 'copy' };
  if (!plain(event)) return undefined;
  switch (event.key) {
    case 'Enter':
      return { kind: 'control' };
    case 'ArrowUp':
      return { kind: 'scroll', lines: -1 };
    case 'ArrowDown':
      return { kind: 'scroll', lines: 1 };
    case 'PageUp':
      return { kind: 'page', pages: -1 };
    case 'PageDown':
      return { kind: 'page', pages: 1 };
    case 'Home':
      return { kind: 'top' };
    case 'End':
      return { kind: 'bottom' };
    default:
      return undefined;
  }
}
