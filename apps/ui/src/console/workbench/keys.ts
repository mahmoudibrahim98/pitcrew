// The workbench's keys, and the palette command for each. No React.
//
// Alt (Option on macOS) with:
// - PageDown / PageUp: the next / previous tab of the active pane;
// - Shift+PageDown / Shift+PageUp: move the tab on screen to the next / previous pane;
// - W: close the tab on screen;
// - \ and Shift+\: split the active pane right / down.
// F6 and Shift+F6 move between panes (the console's, `console-page.tsx`).
//
// Why these: a browser keeps Ctrl+W, Ctrl+Tab, Ctrl+PageDown and Ctrl+1–9 for itself (the page never
// sees them), and the shell owns Ctrl K, J, B and . Alt with PageDown, PageUp, W and \ means
// nothing to Chromium, Firefox, WebKit or the desktop's webviews. In a text field Alt+W and Alt+\
// type characters on macOS, so those two act only outside one; the page keys act everywhere. A
// terminal in control mode, like any key-owning surface, keeps every key.

import { SHELL_KEYS_ATTRIBUTE } from '../../shell/index.ts';

export type WorkbenchAction =
  | 'next-tab'
  | 'previous-tab'
  | 'close-tab'
  | 'next-pane'
  | 'previous-pane'
  | 'split-right'
  | 'split-down'
  | 'move-to-next-pane'
  | 'move-to-previous-pane'
  | 'toggle-details';

/** Each action's palette label and key hint (`Kbd` names). */
export const WORKBENCH_ACTIONS: Record<WorkbenchAction, { label: string; keys?: readonly string[]; keywords?: readonly string[] }> = {
  'next-tab': { label: 'Next tab', keys: ['alt', 'PageDown'], keywords: ['switch', 'workbench'] },
  'previous-tab': { label: 'Previous tab', keys: ['alt', 'PageUp'], keywords: ['switch', 'workbench'] },
  'close-tab': { label: 'Close the tab', keys: ['alt', 'W'], keywords: ['workbench', 'remove'] },
  'next-pane': { label: 'Next pane', keys: ['F6'], keywords: ['focus', 'workbench', 'group'] },
  'previous-pane': { label: 'Previous pane', keys: ['shift', 'F6'], keywords: ['focus', 'workbench', 'group'] },
  'split-right': { label: 'Split the pane right', keys: ['alt', '\\'], keywords: ['side by side', 'workbench', 'vertical'] },
  'split-down': { label: 'Split the pane down', keys: ['alt', 'shift', '\\'], keywords: ['stack', 'workbench', 'horizontal'] },
  'move-to-next-pane': { label: 'Move the tab to the next pane', keys: ['alt', 'shift', 'PageDown'], keywords: ['workbench'] },
  'move-to-previous-pane': {
    label: 'Move the tab to the previous pane',
    keys: ['alt', 'shift', 'PageUp'],
    keywords: ['workbench'],
  },
  'toggle-details': { label: 'Show or hide the session details', keywords: ['sidebar', 'info', 'model', 'workbench'] },
};

interface KeyLike {
  key: string;
  code: string;
  altKey: boolean;
  ctrlKey: boolean;
  metaKey: boolean;
  shiftKey: boolean;
  defaultPrevented: boolean;
  target: EventTarget | null;
}

function isEditable(element: Element): boolean {
  if (element.closest('input, textarea, select') !== null) return true;
  const host = element.closest('[contenteditable]');
  return host !== null && host.getAttribute('contenteditable') !== 'false';
}

/** The workbench action a key press asks for, or undefined to leave it alone. */
export function workbenchActionFor(event: KeyLike): WorkbenchAction | undefined {
  if (event.defaultPrevented || !event.altKey || event.ctrlKey || event.metaKey) return undefined;
  const target = event.target instanceof Element ? event.target : null;
  if (target?.closest(`[${SHELL_KEYS_ATTRIBUTE}="none"]`) != null) return undefined;
  if (event.key === 'PageDown') return event.shiftKey ? 'move-to-next-pane' : 'next-tab';
  if (event.key === 'PageUp') return event.shiftKey ? 'move-to-previous-pane' : 'previous-tab';
  if (target !== null && isEditable(target)) return undefined;
  if (event.code === 'KeyW' && !event.shiftKey) return 'close-tab';
  if (event.code === 'Backslash') return event.shiftKey ? 'split-down' : 'split-right';
  return undefined;
}
