// The terminal's pure parts: the release chord and view-mode keys, links, the theme from the
// design tokens, and dim text that keeps 4.5:1.

import { describe, expect, it } from 'vitest';
import { contrast, dimTextStyles, parseHex, readableDim } from '../terminal/contrast.ts';
import { isReleaseChord, viewModeKey } from '../terminal/keys.ts';
import { httpLink, isLinkClick } from '../terminal/options.ts';
import { terminalFont, terminalTheme } from '../terminal/theme.ts';

const key = (init: Partial<{ key: string; code: string; ctrlKey: boolean; shiftKey: boolean; altKey: boolean; metaKey: boolean }>) => ({
  key: '',
  code: '',
  ctrlKey: false,
  shiftKey: false,
  altKey: false,
  metaKey: false,
  ...init,
});

describe('keys', () => {
  it('Ctrl+Shift+X releases, by its character or, without a Latin X, its position', () => {
    expect(isReleaseChord(key({ key: 'X', code: 'KeyX', ctrlKey: true, shiftKey: true }))).toBe(true);
    expect(isReleaseChord(key({ key: 'Ч', code: 'KeyX', ctrlKey: true, shiftKey: true }))).toBe(true);
    // Dvorak: the X key is elsewhere, and KeyX types Q.
    expect(isReleaseChord(key({ key: 'Q', code: 'KeyX', ctrlKey: true, shiftKey: true }))).toBe(false);
    expect(isReleaseChord(key({ key: 'X', code: 'KeyB', ctrlKey: true, shiftKey: true }))).toBe(true);
    // Not Ctrl+X (the program's), nor with Alt or Meta as well.
    expect(isReleaseChord(key({ key: 'x', code: 'KeyX', ctrlKey: true }))).toBe(false);
    expect(isReleaseChord(key({ key: 'X', code: 'KeyX', ctrlKey: true, shiftKey: true, altKey: true }))).toBe(false);
    expect(isReleaseChord(key({ key: 'X', code: 'KeyX', ctrlKey: true, shiftKey: true, metaKey: true }))).toBe(false);
    expect(isReleaseChord(key({ key: 'X', code: 'KeyX', shiftKey: true, metaKey: true }))).toBe(false);
  });

  it('in view mode: Enter takes control, the scroll keys scroll, and the rest is left alone', () => {
    expect(viewModeKey(key({ key: 'Enter' }))).toEqual({ kind: 'control' });
    expect(viewModeKey(key({ key: 'Enter', ctrlKey: true }))).toBeUndefined();
    expect(viewModeKey(key({ key: 'ArrowUp' }))).toEqual({ kind: 'scroll', lines: -1 });
    expect(viewModeKey(key({ key: 'PageDown' }))).toEqual({ kind: 'page', pages: 1 });
    expect(viewModeKey(key({ key: 'Home' }))).toEqual({ kind: 'top' });
    expect(viewModeKey(key({ key: 'End' }))).toEqual({ kind: 'bottom' });
    for (const other of ['F6', 'Tab', 'Escape', 'a']) expect(viewModeKey(key({ key: other }))).toBeUndefined();
    // The shell's keys.
    expect(viewModeKey(key({ key: 'k', ctrlKey: true }))).toBeUndefined();
    expect(viewModeKey(key({ key: 'b', ctrlKey: true }))).toBeUndefined();
  });
});

describe('links', () => {
  it('only absolute http and https URLs', () => {
    expect(httpLink('https://example.test/a?b=c')).toBe('https://example.test/a?b=c');
    expect(httpLink('http://127.0.0.1:8080')).toBe('http://127.0.0.1:8080/');
    for (const bad of ['javascript:alert(1)', 'file:///etc/passwd', 'data:text/html,x', 'mailto:a@example.test', '/relative', 'not a url', '']) {
      expect(httpLink(bad)).toBeUndefined();
    }
  });

  it('only on a modifier-click', () => {
    expect(isLinkClick({ ctrlKey: false, metaKey: false })).toBe(false);
    expect(isLinkClick({ ctrlKey: true, metaKey: false })).toBe(true);
    expect(isLinkClick({ ctrlKey: false, metaKey: true })).toBe(true);
  });
});

/** A token reader over a fixed set of values, as the root element would give them. */
const reader = (values: Record<string, string>) => (name: string) => values[name] ?? '';

describe('theme', () => {
  it('takes its colours and font from the tokens', () => {
    const read = reader({
      'color-scheme': 'dark',
      '--pc-bg': '#101010',
      '--pc-ink': '#F0F0F0',
      '--pc-risk': '#FF6666',
      '--pc-ok': '#55DD88',
      '--pc-warn': '#EEAA55',
      '--pc-accent': '#8888FF',
      '--pc-font-mono': '"Geist Mono", monospace',
      '--pc-text-md': '13.5px',
    });
    expect(terminalTheme(read)).toMatchObject({
      background: '#101010',
      foreground: '#F0F0F0',
      cursor: '#F0F0F0',
      cursorAccent: '#101010',
      red: '#FF6666',
      green: '#55DD88',
      yellow: '#EEAA55',
      blue: '#8888FF',
      brightWhite: '#F0F0F0',
    });
    expect(terminalFont(read)).toEqual({ fontFamily: '"Geist Mono", monospace', fontSize: 13.5 });
  });

  it('falls back to the light or dark palette when a token is missing', () => {
    expect(terminalTheme(reader({})).background).toBe('#FFFFFF');
    expect(terminalTheme(reader({ 'color-scheme': 'dark' })).background).toBe('#111113');
    expect(terminalFont(reader({})).fontSize).toBe(13.5);
  });

  it('every colour of both palettes reads on its background', () => {
    for (const scheme of ['light', 'dark']) {
      const theme = terminalTheme(reader({ 'color-scheme': scheme }));
      const bg = parseHex(theme.background);
      if (bg === undefined) throw new Error('no background');
      // "black" in a dark theme and "white" in a light one may be low: xterm's minimum contrast
      // (4.5) corrects them as it draws.
      for (const name of ['foreground', 'red', 'green', 'yellow', 'blue', 'magenta', 'cyan', 'brightWhite'] as const) {
        const color = parseHex(theme[name]);
        if (color === undefined) throw new Error(`no ${name}`);
        expect(contrast(color, bg), `${scheme} ${name}`).toBeGreaterThanOrEqual(4.5);
      }
    }
  });
});

describe('dim text', () => {
  const white = [255, 255, 255] as const;
  const black = [17, 17, 19] as const;

  it('is lighter than the text but keeps 4.5:1', () => {
    const ink = [24, 24, 27] as const;
    const dim = readableDim(ink, white);
    expect(contrast(dim, white)).toBeGreaterThanOrEqual(4.5);
    expect(contrast(dim, white)).toBeLessThan(contrast(ink, white));
    const light = [237, 237, 239] as const;
    expect(contrast(readableDim(light, black), black)).toBeGreaterThanOrEqual(4.5);
  });

  it('pushes a colour that is too faint to begin with up to 4.5:1', () => {
    const faint = [200, 200, 200] as const;
    expect(contrast(readableDim(faint, white), white)).toBeGreaterThanOrEqual(4.5);
  });

  it('has a rule for the default colour, all 256 colours and inverse video', () => {
    const css = dimTextStyles(terminalTheme(reader({})), '[data-terminal-focus]');
    const rules = css.split('\n');
    expect(rules).toHaveLength(258);
    expect(rules[0]).toMatch(/^\[data-terminal-focus\] \.xterm-rows span\.xterm-dim:not\(\[class\*="xterm-fg-"\]\)\{color:#[0-9a-f]{6} !important\}$/);
    expect(css).toContain('span.xterm-dim.xterm-fg-255{');
    expect(css).toContain('span.xterm-dim.xterm-fg-257{');
    for (const rule of rules) {
      const color = parseHex(/color:(#[0-9a-f]{6})/.exec(rule)?.[1]);
      if (color === undefined) throw new Error(rule);
      const background = rule.includes('xterm-fg-257') ? [24, 24, 27] as const : white;
      expect(contrast(color, background), rule).toBeGreaterThanOrEqual(4.5);
    }
  });
});
