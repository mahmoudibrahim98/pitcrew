// @vitest-environment happy-dom

import { afterEach, beforeEach, describe, expect, it } from 'vitest';
import { handleShellKey, ownsShellKeys, type ShellAction } from './shortcuts.ts';

let ran: ShellAction[] = [];
const listener = (event: KeyboardEvent) => handleShellKey(event, (action) => ran.push(action));

beforeEach(() => {
  ran = [];
  document.body.innerHTML = `
    <button id="plain">plain</button>
    <input id="input" />
    <textarea id="textarea"></textarea>
    <select id="select"><option>a</option></select>
    <div id="editor" contenteditable="true"><p><span id="inside-editor">text</span></p></div>
    <div contenteditable="true"><span id="not-editable" contenteditable="false">fixed</span></div>
    <div id="surface">
      <div><button id="in-surface">terminal</button><input id="input-in-surface" /></div>
    </div>`;
  for (const [name, value] of Object.entries(ownsShellKeys)) {
    document.getElementById('surface')?.setAttribute(name, value);
  }
  window.addEventListener('keydown', listener);
});

afterEach(() => {
  window.removeEventListener('keydown', listener);
  document.body.innerHTML = '';
});

const CODES: Record<string, string> = { '.': 'Period', k: 'KeyK', j: 'KeyJ', b: 'KeyB', l: 'KeyL' };

/** Presses Ctrl+key on the element; returns whether the shell prevented the default. */
function press(id: string, key: string, init: KeyboardEventInit = {}): boolean {
  const target = document.getElementById(id);
  if (target === null) throw new Error(`no #${id}`);
  const event = new KeyboardEvent('keydown', {
    key,
    code: CODES[key] ?? '',
    ctrlKey: true,
    bubbles: true,
    cancelable: true,
    ...init,
  });
  target.dispatchEvent(event);
  return event.defaultPrevented;
}

describe('shell shortcuts', () => {
  it('run from anywhere else, and keep the browser from acting on them', () => {
    expect(press('plain', 'j')).toBe(true);
    expect(press('plain', 'k')).toBe(true);
    expect(press('plain', 'b')).toBe(true);
    expect(press('plain', '.')).toBe(true);
    expect(ran).toEqual(['orchestrator', 'palette', 'sidebar', 'layout']);
  });

  it('leave editable elements alone, except Ctrl K', () => {
    for (const id of ['input', 'textarea', 'select', 'editor', 'inside-editor']) {
      expect(press(id, 'j'), `${id}: Ctrl J`).toBe(false);
      expect(press(id, 'b'), `${id}: Ctrl B`).toBe(false);
      expect(press(id, '.'), `${id}: Ctrl .`).toBe(false);
      expect(press(id, 'k'), `${id}: Ctrl K`).toBe(true);
    }
    expect(ran).toEqual(['palette', 'palette', 'palette', 'palette', 'palette']);
  });

  it('treat contenteditable="false" as not editable', () => {
    expect(press('not-editable', 'j')).toBe(true);
    expect(ran).toEqual(['orchestrator']);
  });

  it('do nothing inside a key-owning surface, however deep, not even Ctrl K', () => {
    for (const id of ['in-surface', 'input-in-surface']) {
      for (const key of ['k', 'j', 'b', '.']) {
        expect(press(id, key), `${id}: Ctrl ${key}`).toBe(false);
      }
    }
    expect(ran).toEqual([]);
  });

  it('toggle once for a key held down', () => {
    expect(press('plain', 'j')).toBe(true);
    expect(press('plain', 'j', { repeat: true })).toBe(true);
    expect(press('plain', 'j', { repeat: true })).toBe(true);
    expect(ran).toEqual(['orchestrator']);
  });

  it('never prevent a key they do not handle', () => {
    expect(press('plain', 'l')).toBe(false);
    expect(press('plain', 'k', { shiftKey: true })).toBe(false);
    expect(press('plain', 'k', { altKey: true })).toBe(false);
    expect(press('plain', 'k', { ctrlKey: false })).toBe(false);
    expect(ran).toEqual([]);
  });

  it('leave a key alone that something else already handled', () => {
    const input = document.getElementById('plain');
    const stop = (event: Event) => event.preventDefault();
    input?.addEventListener('keydown', stop);
    press('plain', 'k');
    input?.removeEventListener('keydown', stop);
    expect(ran).toEqual([]);
  });
});
