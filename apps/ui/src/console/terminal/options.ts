// xterm's options for output that comes from an agent and whatever it ran, so is hostile:
// no proposed API, no window reports, a bounded scrollback, links only for http and https on a
// modifier-click with a bounded length, and no OSC titles or clipboard (see `SWALLOWED_OSC`).
// The console README explains each choice.

import type { ILinkHandler, ITerminalOptions, ITheme } from '@xterm/xterm';
import type { TerminalFont } from './theme.ts';

/**
 * Lines kept above the screen. This bounds the number of rows, not every byte they hold: a cell
 * can carry any number of combining marks. Links are bounded on their own (`MAX_LINK_LENGTH`).
 */
export const SCROLLBACK = 5_000;

/**
 * The longest OSC 8 link target kept. xterm accepts targets of up to 10 MB and keeps every link
 * without an `id` with its line; a longer one is dropped, and its text shows as plain text.
 */
export const MAX_LINK_LENGTH = 2048;

/** Whether an OSC 8 payload (`params;uri`) has a target longer than `MAX_LINK_LENGTH`. */
export function isOversizedLink(data: string): boolean {
  const separator = data.indexOf(';');
  return separator !== -1 && data.length - separator - 1 > MAX_LINK_LENGTH;
}

/**
 * OSC sequences dropped before xterm acts on them: 0, 1 and 2 set the title (it never reaches
 * the document's), 52 writes the clipboard. xterm has no clipboard handling without its addon,
 * which is never loaded; dropping 52 as well keeps it that way if that ever changes.
 */
export const SWALLOWED_OSC = [0, 1, 2, 52] as const;

/** The URL if it is absolute http or https, else undefined. */
export function httpLink(raw: string): string | undefined {
  let url: URL;
  try {
    url = new URL(raw);
  } catch {
    return undefined;
  }
  return url.protocol === 'http:' || url.protocol === 'https:' ? url.href : undefined;
}

/** Whether a click asks to follow a link: Ctrl (Cmd on macOS) held. */
export function isLinkClick(event: Pick<MouseEvent, 'ctrlKey' | 'metaKey'>): boolean {
  return event.ctrlKey || event.metaKey;
}

/**
 * OSC 8 hyperlinks: only http and https, opened by `open` (the console's opener) on a
 * modifier-click; a plain click does nothing. `hint` gets the link under the mouse, to show it.
 */
export function linkHandler(open: (url: string) => void, hint: (url: string | undefined) => void): ILinkHandler {
  return {
    allowNonHttpProtocols: false,
    activate(event, text) {
      const href = httpLink(text);
      if (href === undefined || !isLinkClick(event)) return;
      event.preventDefault();
      open(href);
    },
    hover(_event, text) {
      hint(httpLink(text));
    },
    leave() {
      hint(undefined);
    },
  };
}

export interface OptionsInput {
  theme: ITheme;
  font: TerminalFont;
  screenReader: boolean;
  links: ILinkHandler;
}

export function terminalOptions(input: OptionsInput): ITerminalOptions {
  return {
    allowProposedApi: false,
    allowTransparency: false,
    // View mode is the default: nothing reaches the program until control is taken.
    disableStdin: true,
    scrollback: SCROLLBACK,
    screenReaderMode: input.screenReader,
    linkHandler: input.links,
    // Every window manipulation and report (title reports included) stays off.
    windowOptions: {},
    // Whatever colours a program picks stay readable (WCAG AA).
    minimumContrastRatio: 4.5,
    // Unknown sequences in hostile output must not flood the console.
    logLevel: 'off',
    cursorBlink: false,
    cursorStyle: 'block',
    cursorInactiveStyle: 'outline',
    fontFamily: input.font.fontFamily,
    fontSize: input.font.fontSize,
    theme: input.theme,
  };
}
