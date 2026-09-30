// The modifier key for shortcuts: Cmd on macOS, Ctrl elsewhere.

function detectMac(): boolean {
  if (typeof navigator === 'undefined') return false;
  const data = (navigator as Navigator & { userAgentData?: { platform?: string } }).userAgentData;
  const platform = data?.platform ?? navigator.platform ?? '';
  return /mac|iphone|ipad/i.test(platform || navigator.userAgent);
}

export const isMac = detectMac();

/** Whether the platform's shortcut modifier, and only it, is held. */
export function hasMod(event: KeyboardEvent | { ctrlKey: boolean; metaKey: boolean; altKey: boolean }): boolean {
  if (event.altKey) return false;
  return isMac ? event.metaKey && !event.ctrlKey : event.ctrlKey && !event.metaKey;
}

/** How a key reads on this platform, for hints: `mod` is Ctrl or ⌘. */
export function keyLabel(key: string): string {
  switch (key.toLowerCase()) {
    case 'mod':
      return isMac ? '⌘' : 'Ctrl';
    case 'shift':
      return isMac ? '⇧' : 'Shift';
    case 'alt':
      return isMac ? '⌥' : 'Alt';
    case 'enter':
      return 'Enter';
    case 'esc':
      return 'Esc';
    default:
      return key.length === 1 ? key.toUpperCase() : key;
  }
}

/** For `aria-keyshortcuts`: `['mod', 'k']` → `Control+K` (or `Meta+K`). */
export function ariaShortcut(keys: readonly string[]): string {
  return keys
    .map((key) => {
      const lower = key.toLowerCase();
      if (lower === 'mod') return isMac ? 'Meta' : 'Control';
      if (lower === 'shift') return 'Shift';
      if (lower === 'alt') return 'Alt';
      if (key === '.') return 'Period';
      return key.length === 1 ? key.toUpperCase() : key;
    })
    .join('+');
}
