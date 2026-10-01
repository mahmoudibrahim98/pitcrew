// Dim text (SGR 2) that still reads. xterm draws dim text at half its colour's opacity, and asks
// only half the minimum contrast of it; on a light background that is under WCAG AA's 4.5:1 even
// for black. For xterm's DOM renderer (the fallback without WebGL) these rules draw dim text
// halfway to the background only as far as 4.5:1 allows. The WebGL renderer draws on a canvas
// that CSS cannot reach, so there dim text stays as xterm draws it.

import type { ITheme } from '@xterm/xterm';

type Rgb = readonly [number, number, number];

/** The minimum contrast for text (WCAG AA). */
const AA = 4.5;

export function parseHex(color: string | undefined): Rgb | undefined {
  const match = /^#([0-9a-f]{2})([0-9a-f]{2})([0-9a-f]{2})(?:[0-9a-f]{2})?$/i.exec(color ?? '');
  if (match === null) return undefined;
  return [Number.parseInt(match[1] ?? '0', 16), Number.parseInt(match[2] ?? '0', 16), Number.parseInt(match[3] ?? '0', 16)];
}

function hex(rgb: Rgb): string {
  return `#${rgb.map((c) => Math.round(c).toString(16).padStart(2, '0')).join('')}`;
}

function luminance(rgb: Rgb): number {
  const [r, g, b] = rgb.map((c) => {
    const s = c / 255;
    return s <= 0.03928 ? s / 12.92 : ((s + 0.055) / 1.055) ** 2.4;
  }) as [number, number, number];
  return 0.2126 * r + 0.7152 * g + 0.0722 * b;
}

export function contrast(a: Rgb, b: Rgb): number {
  const [hi, lo] = [luminance(a), luminance(b)].sort((x, y) => y - x) as [number, number];
  return (hi + 0.05) / (lo + 0.05);
}

/** `from` moved `t` of the way to `to`, in whole channel values (as it will be written). */
function mix(from: Rgb, to: Rgb, t: number): Rgb {
  return [0, 1, 2].map((i) => Math.round((from[i] ?? 0) + ((to[i] ?? 0) - (from[i] ?? 0)) * t)) as unknown as Rgb;
}

/**
 * A dim version of `fg` on `bg`: as close to halfway to the background as keeps 4.5:1. A colour
 * that does not reach 4.5:1 itself is first pushed away from the background until it does.
 */
export function readableDim(fg: Rgb, bg: Rgb): Rgb {
  let base = fg;
  if (contrast(base, bg) < AA) {
    const away: Rgb = luminance(bg) > 0.5 ? [0, 0, 0] : [255, 255, 255];
    for (let t = 0.05; t <= 1 && contrast(base, bg) < AA; t += 0.05) base = mix(fg, away, t);
    return base;
  }
  for (let t = 0.5; t > 0; t -= 0.05) {
    const dim = mix(base, bg, t);
    if (contrast(dim, bg) >= AA) return dim;
  }
  return base;
}

const ANSI_NAMES = [
  'black',
  'red',
  'green',
  'yellow',
  'blue',
  'magenta',
  'cyan',
  'white',
  'brightBlack',
  'brightRed',
  'brightGreen',
  'brightYellow',
  'brightBlue',
  'brightMagenta',
  'brightCyan',
  'brightWhite',
] as const satisfies readonly (keyof ITheme)[];

/** The 256-colour palette after the theme's 16: a 6×6×6 cube, then 24 greys (as xterm has it). */
function extendedColor(index: number): Rgb {
  if (index < 232) {
    const levels = [0, 95, 135, 175, 215, 255];
    const n = index - 16;
    return [levels[Math.floor(n / 36)] ?? 0, levels[Math.floor(n / 6) % 6] ?? 0, levels[n % 6] ?? 0];
  }
  const grey = 8 + (index - 232) * 10;
  return [grey, grey, grey];
}

/** xterm's class for the default background drawn as a foreground (inverse video). */
const INVERTED_DEFAULT = 257;

/** CSS for dim text in xterm's DOM renderer, under `scope`; empty if the theme is not plain hex. */
export function dimTextStyles(theme: ITheme, scope: string): string {
  const bg = parseHex(theme.background);
  const fg = parseHex(theme.foreground);
  if (bg === undefined || fg === undefined) return '';
  const rows = `${scope} .xterm-rows span.xterm-dim`;
  // `!important`: xterm sets its own (half-contrast) colour inline on some dim cells.
  const rule = (selector: string, color: Rgb) => `${selector}{color:${hex(color)} !important}`;
  const rules = [rule(`${rows}:not([class*="xterm-fg-"])`, readableDim(fg, bg))];
  for (let i = 0; i < 256; i += 1) {
    const name = ANSI_NAMES[i];
    const color = name === undefined ? extendedColor(i) : parseHex(theme[name]);
    if (color !== undefined) rules.push(rule(`${rows}.xterm-fg-${i}`, readableDim(color, bg)));
  }
  // Inverse video: the background colour as text on the foreground colour.
  rules.push(rule(`${rows}.xterm-fg-${INVERTED_DEFAULT}`, readableDim(bg, fg)));
  return rules.join('\n');
}
