// The terminal's colours and font, from the design tokens (`@pitcrew/tokens`), read off the root
// element so a theme switch (or the system's) is picked up by reading again. Colours the tokens
// have (background, ink, the accent, risk, ok and warn) come from them; the rest of the ANSI
// palette is chosen to read on the token background, and xterm's minimum contrast ratio corrects
// whatever colours a program asks for.

import type { ITheme } from '@xterm/xterm';

/** Reads a CSS property of the root element: a token (`--pc-bg`) or `color-scheme`. */
export type TokenReader = (name: string) => string;

export const rootTokens: TokenReader = (name) =>
  getComputedStyle(document.documentElement).getPropertyValue(name).trim();

/** `#RRGGBB` with an alpha channel; other colours are returned as they are. */
function withAlpha(color: string, alpha: number): string {
  if (!/^#[0-9a-f]{6}$/i.test(color)) return color;
  const byte = Math.round(Math.min(1, Math.max(0, alpha)) * 255);
  return `${color}${byte.toString(16).padStart(2, '0')}`;
}

const LIGHT = {
  bg: '#FFFFFF',
  ink: '#18181B',
  ink2: '#52525B',
  muted: '#85858F',
  accent: '#5B5BD6',
  accentText: '#4141B5',
  risk: '#D02626',
  ok: '#15803D',
  warn: '#B45F06',
  magenta: '#A21CAF',
  cyan: '#0E7490',
  brightRed: '#B91C1C',
  brightGreen: '#166534',
  brightYellow: '#92400E',
  brightMagenta: '#86198F',
  brightCyan: '#155E75',
};

const DARK: typeof LIGHT = {
  bg: '#111113',
  ink: '#EDEDEF',
  ink2: '#B4B4BB',
  muted: '#82828C',
  accent: '#8B8CF8',
  accentText: '#C4C5FD',
  risk: '#F87171',
  ok: '#4ADE80',
  warn: '#F2A65A',
  magenta: '#E879F9',
  cyan: '#22D3EE',
  brightRed: '#FCA5A5',
  brightGreen: '#86EFAC',
  brightYellow: '#FCD34D',
  brightMagenta: '#F0ABFC',
  brightCyan: '#67E8F9',
};

export function isDarkScheme(read: TokenReader): boolean {
  return /\bdark\b/.test(read('color-scheme'));
}

export function terminalTheme(read: TokenReader): ITheme {
  const dark = isDarkScheme(read);
  const fixed = dark ? DARK : LIGHT;
  const token = (name: string, fallback: string) => read(name) || fallback;
  const bg = token('--pc-bg', fixed.bg);
  const ink = token('--pc-ink', fixed.ink);
  const ink2 = token('--pc-ink-2', fixed.ink2);
  const muted = token('--pc-muted', fixed.muted);
  const accent = token('--pc-accent', fixed.accent);
  const accentText = token('--pc-accent-text', fixed.accentText);
  const risk = token('--pc-risk', fixed.risk);
  const ok = token('--pc-ok', fixed.ok);
  const warn = token('--pc-warn', fixed.warn);
  return {
    background: bg,
    foreground: ink,
    cursor: ink,
    cursorAccent: bg,
    selectionBackground: withAlpha(accent, 0.35),
    selectionInactiveBackground: withAlpha(accent, 0.2),
    scrollbarSliderBackground: withAlpha(muted, 0.35),
    scrollbarSliderHoverBackground: withAlpha(muted, 0.55),
    scrollbarSliderActiveBackground: withAlpha(muted, 0.7),
    // In a light theme "black" is the ink and "white" a dark grey, so neither vanishes.
    black: dark ? muted : ink,
    red: risk,
    green: ok,
    yellow: warn,
    blue: dark ? accent : accentText,
    magenta: fixed.magenta,
    cyan: fixed.cyan,
    white: ink2,
    brightBlack: dark ? muted : ink2,
    brightRed: fixed.brightRed,
    brightGreen: fixed.brightGreen,
    brightYellow: fixed.brightYellow,
    brightBlue: dark ? accentText : accent,
    brightMagenta: fixed.brightMagenta,
    brightCyan: fixed.brightCyan,
    brightWhite: ink,
  };
}

export interface TerminalFont {
  fontFamily: string;
  fontSize: number;
}

export function terminalFont(read: TokenReader): TerminalFont {
  const size = Number.parseFloat(read('--pc-text-md'));
  return {
    fontFamily: read('--pc-font-mono') || 'ui-monospace, "Cascadia Mono", Consolas, monospace',
    fontSize: Number.isFinite(size) && size > 0 ? size : 13.5,
  };
}
