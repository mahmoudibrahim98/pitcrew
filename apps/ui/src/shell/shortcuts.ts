// The shell's global shortcuts, with Cmd in place of Ctrl on macOS:
// Ctrl . switches layout, Ctrl K opens the palette, Ctrl J the Orchestrator, Ctrl B the sidebar.
//
// They stay out of the way of whatever has focus:
// - inside a key-owning surface (an element with `data-shell-keys="none"`, or inside one: the
//   terminal, an editor), no shell shortcut fires and nothing is prevented;
// - inside an editable element (input, textarea, select, contenteditable), only Ctrl K fires, and
//   the other keys reach the element;
// - a held-down key toggles nothing after its first press;
// - a key the shell does not handle is never prevented.

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

export type ShellAction = keyof typeof SHORTCUTS;

/** The attribute a feature puts on a container whose keys the shell must leave alone. */
export const SHELL_KEYS_ATTRIBUTE = 'data-shell-keys';

/** Spread on a key-owning container: `<div {...ownsShellKeys}>`. */
export const ownsShellKeys = { [SHELL_KEYS_ATTRIBUTE]: 'none' } as const;

const ACTIONS: Record<string, ShellAction> = { '.': 'layout', k: 'palette', j: 'orchestrator', b: 'sidebar' };

function isEditable(element: Element): boolean {
  if (element.closest('input, textarea, select') !== null) return true;
  // The nearest contenteditable decides: an island marked "false" inside an editor is not editable.
  const host = element.closest('[contenteditable]');
  return host !== null && host.getAttribute('contenteditable') !== 'false';
}

interface KeyLike {
  key: string;
  code: string;
  ctrlKey: boolean;
  metaKey: boolean;
  altKey: boolean;
  shiftKey: boolean;
  defaultPrevented: boolean;
  target: EventTarget | null;
}

/** The shell action a key press asks for, or undefined when the shell must leave it alone. */
export function shellActionFor(event: KeyLike): ShellAction | undefined {
  if (event.defaultPrevented || event.shiftKey || !hasMod(event)) return undefined;
  const action = ACTIONS[event.code === 'Period' ? '.' : event.key.toLowerCase()];
  if (action === undefined) return undefined;
  const target = event.target instanceof Element ? event.target : null;
  if (target === null) return action;
  if (target.closest(`[${SHELL_KEYS_ATTRIBUTE}="none"]`) !== null) return undefined;
  if (action !== 'palette' && isEditable(target)) return undefined;
  return action;
}

/**
 * Handles a keydown for the shell: keeps the browser from also acting on a shell shortcut, and
 * runs it unless the key is being held down.
 */
export function handleShellKey(event: KeyboardEvent, run: (action: ShellAction) => void): void {
  const action = shellActionFor(event);
  if (action === undefined) return;
  event.preventDefault();
  if (!event.repeat) run(action);
}

export function runShellAction(action: ShellAction, router: AnyRouter, ws: string): void {
  const shell = useShell.getState();
  if (action === 'layout') switchLayout(router, ws);
  else if (action === 'palette') shell.setPaletteOpen(!shell.paletteOpen);
  else if (action === 'orchestrator') shell.setOrchestratorOpen(!shell.orchestratorOpen);
  else shell.setSidebarCollapsed(!shell.sidebarCollapsed);
}

export function useShellShortcuts(router: AnyRouter, ws: string): void {
  useEffect(() => {
    const onKeyDown = (event: KeyboardEvent) =>
      handleShellKey(event, (action) => runShellAction(action, router, ws));
    window.addEventListener('keydown', onKeyDown);
    return () => window.removeEventListener('keydown', onKeyDown);
  }, [router, ws]);
}
