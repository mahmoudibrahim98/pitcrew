// @vitest-environment happy-dom
// @vitest-environment-options {"url": "http://localhost:5173/"}

// The workbench as the console mounts it, against the mock hub: preview tabs from the list, splits
// with a view per pane, the URL following the tab on screen, the layout kept per workspace (and
// a corrupt one ignored), the keys and their palette commands, the details sidebar with its files,
// unsaved edits across tab switches, and dragging tabs.

import { act, fireEvent, screen, within } from '@testing-library/react';
import { createMemoryHistory, RouterProvider } from '@tanstack/react-router';
import { afterAll, afterEach, beforeAll, beforeEach, describe, expect, it, vi } from 'vitest';

// happy-dom cannot draw a terminal; terminal-view.test.tsx and the Playwright specs test xterm.
vi.mock('@xterm/xterm', () => ({
  Terminal: class {
    options: Record<string, unknown> = {};
    cols = 80;
    rows = 24;
    textarea: HTMLTextAreaElement | undefined;
    parser = { registerOscHandler: () => ({ dispose() {} }) };
    attachCustomKeyEventHandler() {}
    onData = () => ({ dispose() {} });
    onBinary = () => ({ dispose() {} });
    onResize = () => ({ dispose() {} });
    loadAddon() {}
    open(parent: HTMLElement) {
      this.textarea = document.createElement('textarea');
      parent.append(this.textarea);
    }
    write() {}
    focus() {}
    dispose() {
      this.textarea?.remove();
    }
  },
}));
vi.mock('@xterm/addon-fit', () => ({
  FitAddon: class {
    activate() {}
    dispose() {}
    fit() {}
  },
}));
vi.mock('@xterm/addon-webgl', () => ({
  WebglAddon: class {
    constructor() {
      throw new Error('WebGL2 not supported');
    }
    activate() {}
    dispose() {}
  },
}));
vi.mock('@xterm/xterm/css/xterm.css', () => ({}));
import { paths, type CommandContext } from '../../shell/index.ts';
import { createAppRouter } from '../../shell/routes.tsx';
import { initialShellState, useShell } from '../../shell/store.ts';
import { feature } from '../index.ts';
import { initialPanes, usePanes } from '../panes.ts';
import { clearDrafts } from '../workbench/drafts.ts';
import { STORAGE_PREFIX, resetWorkbenchStores } from '../workbench/store.ts';
import { eventually, ID, renderWithHub, startHub, stubLayout, unmountAndSettle, type HubProcess } from './harness.tsx';

const WS = '01JB000000000000000WSP0001';

let hub: HubProcess;
beforeAll(async () => {
  hub = await startHub();
});
afterAll(async () => {
  await hub.close();
});

let unstub: () => void = () => {};
beforeEach(() => {
  unstub = stubLayout({ viewport: 2_000, row: 56 });
});
afterEach(async () => {
  await unmountAndSettle();
  unstub();
  vi.restoreAllMocks();
  vi.unstubAllGlobals();
  localStorage.clear();
  resetWorkbenchStores();
  clearDrafts();
  usePanes.setState(initialPanes);
  useShell.setState(initialShellState);
});

function renderConsole(path: string) {
  const router = createAppRouter([feature], { history: createMemoryHistory({ initialEntries: [path] }) });
  const result = renderWithHub(hub, <RouterProvider router={router} />, { strict: true });
  const context: CommandContext = {
    workspace: WS,
    go: (to) => void router.navigate({ href: paths.under(WS, to) }),
    switchLayout: () => {},
    create: () => {},
  };
  const run = (id: string) => {
    const command = feature.commands?.find((c) => c.id === id);
    if (command === undefined) throw new Error(`no command ${id}`);
    act(() => command.run(context));
  };
  return { ...result, router, run, location: () => router.state.location };
}

const rowOf = (id: string) => document.querySelector<HTMLElement>(`[data-session="${id}"]`) as HTMLElement;
const pane = (n: number) => screen.getByRole('region', { name: `Pane ${n}` });
const tabsIn = (n: number) => within(screen.getByRole('tablist', { name: `Tabs in pane ${n}` })).queryAllByRole('tab');
/** Each pane's tab names, a `*` marking the one on screen in that pane. */
const picture = () =>
  screen.queryAllByRole('tablist').map((list) =>
    within(list)
      .queryAllByRole('tab')
      .map((tab) => `${(tab.textContent ?? '').trim()}${tab.getAttribute('aria-selected') === 'true' ? '*' : ''}`),
  );
const focusedPane = () => document.activeElement?.closest<HTMLElement>('[data-pane]')?.dataset.pane;

describe('the workbench', () => {
  it('opens a session from the list as a preview, which the next replaces; Enter keeps it', async () => {
    const { location } = renderConsole(paths.console(WS));
    await screen.findByText('Choose a session', undefined, { timeout: 8_000 });
    await eventually(() => expect(rowOf(ID.ses1)).not.toBeNull());

    fireEvent.click(rowOf(ID.ses1));
    await eventually(() => expect(picture()).toEqual([['Draft method section(preview)*']]));
    expect(location().pathname).toBe(paths.session(WS, ID.ses1));
    fireEvent.click(rowOf(ID.ses2));
    await eventually(() => expect(picture()).toEqual([['Seed runs 1–5(preview)*']]));
    expect(location().pathname).toBe(paths.session(WS, ID.ses2));

    // Enter on the list keeps it; the next one opens beside it.
    const listbox = screen.getByRole('listbox', { name: 'Sessions' });
    listbox.focus();
    fireEvent.keyDown(listbox, { key: 'Enter' });
    await eventually(() => expect(picture()).toEqual([['Seed runs 1–5*']]));
    fireEvent.click(rowOf(ID.ses4));
    await eventually(() => expect(picture()).toEqual([['Seed runs 1–5', 'Codex rollout parser(preview)*']]));
    // Choosing one that is open shows it; nothing new.
    fireEvent.click(rowOf(ID.ses2));
    await eventually(() => expect(picture()).toEqual([['Seed runs 1–5*', 'Codex rollout parser(preview)']]));
    expect(location().pathname).toBe(paths.session(WS, ID.ses2));
  }, 20_000);

  it('splits; each pane has its own view; the URL follows the tab on screen', async () => {
    const { location } = renderConsole(paths.session(WS, ID.ses1));
    await eventually(() => expect(picture()).toEqual([['Draft method section(preview)*']]), { timeout: 8_000 });
    fireEvent.click(within(pane(1)).getByRole('button', { name: 'Split right' }));
    await eventually(() => expect(picture()).toEqual([['Draft method section*'], ['Draft method section*']]));
    // The copy is in focus, and the pane it is in is the active one.
    await eventually(() => expect(document.activeElement).toBe(tabsIn(2)[0]));
    expect(pane(2).hasAttribute('data-active-group')).toBe(true);

    fireEvent.click(within(pane(2)).getByRole('radio', { name: 'Terminal' }));
    await eventually(() => expect(location().search).toEqual({ view: 'terminal' }));
    await within(pane(2)).findByRole('group', { name: 'Terminal' });
    expect(within(pane(1)).getByRole('group', { name: 'Transcript' })).toBeTruthy();
    expect(picture()).toEqual([['Draft method section*'], ['Draft method section· Terminal*']]);
    // Each pane's landmarks keep their own names.
    expect(within(pane(1)).getByRole('region', { name: 'Chat' })).toBeTruthy();

    // Back in pane 1, the URL shows its chat.
    fireEvent.pointerDown(tabsIn(1)[0] as HTMLElement);
    fireEvent.click(tabsIn(1)[0] as HTMLElement);
    await eventually(() => expect(location().search).toEqual({}));
    expect(pane(1).hasAttribute('data-active-group')).toBe(true);

    // F6 goes through the panes in order: the list, pane 1's transcript and composer, pane 2's terminal.
    const listbox = screen.getByRole('listbox', { name: 'Sessions' });
    listbox.focus();
    const active = () => document.activeElement as HTMLElement;
    fireEvent.keyDown(active(), { key: 'F6' });
    expect(focusedPane()).toBe('chat');
    fireEvent.keyDown(active(), { key: 'F6' });
    expect(focusedPane()).toBe('composer');
    fireEvent.keyDown(active(), { key: 'F6' });
    expect(focusedPane()).toBe('terminal');
    expect(active().closest('[data-group]')).toBe(pane(2));
  }, 20_000);

  it('keeps the layout per workspace across a reload, and starts afresh from a corrupt one', async () => {
    const first = renderConsole(paths.session(WS, ID.ses1));
    await eventually(() => expect(picture()).toEqual([['Draft method section(preview)*']]), { timeout: 8_000 });
    fireEvent.click(within(pane(1)).getByRole('button', { name: 'Split down' }));
    await eventually(() => expect(picture()).toHaveLength(2));
    const stored = localStorage.getItem(`${STORAGE_PREFIX}${WS}`);
    expect(stored).not.toBeNull();
    expect(JSON.parse(stored ?? '{}')).toMatchObject({ version: 1, layout: { root: { type: 'split', direction: 'column' } } });
    first.unmount();

    // A new page: the store is read from storage again.
    resetWorkbenchStores();
    renderConsole(paths.console(WS));
    await eventually(() => expect(picture()).toEqual([['Draft method section*'], ['Draft method section*']]), { timeout: 8_000 });
    expect(pane(2).hasAttribute('data-active-group')).toBe(true);
    await unmountAndSettle();

    for (const corrupt of ['{not json', JSON.stringify({ version: 1, layout: { root: { type: 'split', children: [] } } })]) {
      localStorage.setItem(`${STORAGE_PREFIX}${WS}`, corrupt);
      resetWorkbenchStores();
      renderConsole(paths.console(WS));
      await screen.findByText('Choose a session', undefined, { timeout: 8_000 });
      expect(screen.queryAllByRole('tab')).toHaveLength(0);
      await unmountAndSettle();
    }
  }, 30_000);

  it('switches, moves and closes tabs from the keys, and from the palette', async () => {
    const { run, location } = renderConsole(paths.console(WS));
    await eventually(() => expect(rowOf(ID.ses1)).not.toBeNull(), { timeout: 8_000 });
    for (const id of [ID.ses1, ID.ses4]) {
      fireEvent.click(rowOf(id));
      await eventually(() => expect(location().pathname).toBe(paths.session(WS, id)));
      fireEvent.doubleClick(tabsIn(1).find((t) => t.getAttribute('aria-selected') === 'true') as HTMLElement);
    }
    await eventually(() => expect(picture()).toEqual([['Draft method section', 'Codex rollout parser*']]));
    const tab = () => tabsIn(1).find((t) => t.getAttribute('aria-selected') === 'true') as HTMLElement;

    // The tab strip's own keys.
    tab().focus();
    fireEvent.keyDown(tab(), { key: 'ArrowLeft' });
    await eventually(() => expect(picture()).toEqual([['Draft method section*', 'Codex rollout parser']]));
    await eventually(() => expect(document.activeElement).toBe(tab()));
    expect(location().pathname).toBe(paths.session(WS, ID.ses1));
    fireEvent.keyDown(tab(), { key: 'ArrowRight', shiftKey: true });
    await eventually(() => expect(picture()).toEqual([['Codex rollout parser', 'Draft method section*']]));

    // Alt PageUp, from the transcript; Alt W does nothing in a text field, but does outside one.
    const transcript = await within(pane(1)).findByRole('group', { name: 'Transcript' });
    fireEvent.keyDown(transcript, { key: 'PageUp', code: 'PageUp', altKey: true });
    await eventually(() => expect(picture()).toEqual([['Codex rollout parser*', 'Draft method section']]));
    const composer = await within(pane(1)).findByRole('textbox', { name: /message/i });
    fireEvent.keyDown(composer, { key: '∑', code: 'KeyW', altKey: true });
    expect(picture()).toEqual([['Codex rollout parser*', 'Draft method section']]);
    // Alt \ splits; Alt Shift PageUp moves the copy back, where the same tab is already.
    fireEvent.keyDown(tab(), { key: '\\', code: 'Backslash', altKey: true });
    await eventually(() => expect(picture()).toHaveLength(2));
    fireEvent.keyDown(tabsIn(2)[0] as HTMLElement, { key: 'PageUp', code: 'PageUp', altKey: true, shiftKey: true });
    await eventually(() => expect(picture()).toEqual([['Codex rollout parser*', 'Draft method section']]));
    fireEvent.keyDown(tab(), { key: 'w', code: 'KeyW', altKey: true });
    await eventually(() => expect(picture()).toEqual([['Draft method section*']]));

    // Every key has a palette command, shown in the console's layout.
    const ids = feature.commands?.filter((c) => c.group === 'Workbench').map((c) => c.id) ?? [];
    expect(ids).toEqual(
      expect.arrayContaining(['console-workbench-next-tab', 'console-workbench-close-tab', 'console-workbench-split-right']),
    );
    expect(feature.commands?.find((c) => c.id === 'console-workbench-next-tab')?.keys).toEqual(['alt', 'PageDown']);
    run('console-workbench-split-down');
    await eventually(() => expect(picture()).toEqual([['Draft method section*'], ['Draft method section*']]));
    expect(pane(2).hasAttribute('data-active-group')).toBe(true);
    run('console-workbench-move-to-previous-pane');
    await eventually(() => expect(picture()).toEqual([['Draft method section*']]));
    run('console-workbench-toggle-details');
    await screen.findByRole('complementary', { name: 'Details' });
    run('console-workbench-close-tab');
    await screen.findByText('Choose a session');
    await eventually(() => expect(location().pathname).toBe(paths.console(WS)));
  }, 30_000);

  it('shows the details, opens a file, keeps its unsaved edit across tabs, and asks before closing it', async () => {
    renderConsole(paths.session(WS, ID.ses1));
    await eventually(() => expect(picture()).toEqual([['Draft method section(preview)*']]), { timeout: 8_000 });
    fireEvent.click(within(pane(1)).getByRole('button', { name: 'Details' }));
    const details = await screen.findByRole('complementary', { name: 'Details' });
    const value = (term: string) => within(details).getByText(term, { selector: 'dt' }).nextElementSibling?.textContent;
    await eventually(() => expect(value('Task')).toMatch(/^PAP-1 · /));
    await eventually(() => expect(value('Workstream')).toBe('Submission'));
    await eventually(() => expect(value('Machine')).toBe('This laptop'));
    await eventually(() => expect(value('Model')).toBe("The CLI's default (Writer)"));
    expect(value('Account')).toBe('Not reported');
    expect(value('State')).toBe('Working');
    // Hand off is shown, and says why it does nothing yet.
    const handOff = within(details).getByRole('button', { name: 'Hand off' });
    expect(handOff.getAttribute('aria-disabled')).toBe('true');
    expect(document.getElementById(handOff.getAttribute('aria-describedby') ?? '')?.textContent).toMatch(/not available yet/);

    const tree = await within(details).findByRole('navigation', { name: 'Folder tree' });
    fireEvent.click(await within(tree).findByRole('button', { name: '▸ src' }));
    fireEvent.click(await within(tree).findByRole('button', { name: 'hello.txt' }));
    await eventually(() => expect(picture()).toEqual([['Draft method section(preview)', 'hello.txt*']]));
    fireEvent.click(await within(pane(1)).findByRole('button', { name: 'Edit' }));
    fireEvent.change(within(pane(1)).getByLabelText('Edit file text'), { target: { value: 'draft text\n' } });
    await eventually(() => expect(picture()).toEqual([['Draft method section(preview)', 'hello.txt●(unsaved)*']]));

    // The session's tab and back: the edit is still there.
    fireEvent.click(tabsIn(1)[0] as HTMLElement);
    await within(pane(1)).findByRole('group', { name: 'Transcript' });
    fireEvent.click(tabsIn(1)[1] as HTMLElement);
    const editor = (await within(pane(1)).findByLabelText('Edit file text')) as HTMLTextAreaElement;
    expect(editor.value).toBe('draft text\n');

    // Closing asks; no keeps it, yes closes it.
    const confirm = vi.fn().mockReturnValue(false);
    vi.stubGlobal('confirm', confirm);
    fireEvent.keyDown(tabsIn(1)[1] as HTMLElement, { key: 'Delete' });
    expect(confirm).toHaveBeenCalledWith('Discard unsaved changes to this file?');
    expect(picture()).toEqual([['Draft method section(preview)', 'hello.txt●(unsaved)*']]);
    confirm.mockReturnValue(true);
    fireEvent.keyDown(tabsIn(1)[1] as HTMLElement, { key: 'Delete' });
    await eventually(() => expect(picture()).toEqual([['Draft method section(preview)*']]));
  }, 30_000);

  it('drags a tab into another pane, and onto an edge to split', async () => {
    renderConsole(paths.session(WS, ID.ses1));
    await eventually(() => expect(picture()).toEqual([['Draft method section(preview)*']]), { timeout: 8_000 });
    fireEvent.click(rowOf(ID.ses4));
    await eventually(() => expect(picture()).toEqual([['Codex rollout parser(preview)*']]));
    fireEvent.doubleClick(tabsIn(1)[0] as HTMLElement);
    fireEvent.click(rowOf(ID.ses1));
    await eventually(() => expect(picture()).toEqual([['Codex rollout parser', 'Draft method section(preview)*']]));

    // happy-dom's DragEvent carries no pointer position: a mouse event with a data transfer does.
    const drag = (type: string, target: Element, x = 0, y = 0) => {
      const event = new MouseEvent(type, { bubbles: true, cancelable: true, clientX: x, clientY: y });
      Object.defineProperty(event, 'dataTransfer', {
        value: { setData() {}, getData: () => '', effectAllowed: 'all', dropEffect: 'none' },
      });
      act(() => {
        target.dispatchEvent(event);
      });
      return event;
    };
    // Onto the right edge of its own pane: a new pane on the right.
    const panel = within(pane(1)).getByRole('tabpanel');
    vi.spyOn(panel, 'getBoundingClientRect').mockReturnValue(new DOMRect(0, 0, 400, 300));
    drag('dragstart', tabsIn(1)[0] as HTMLElement);
    expect(drag('dragover', panel, 390, 150).defaultPrevented).toBe(true);
    expect(panel.getAttribute('data-drop-zone')).toBe('right');
    drag('drop', panel, 390, 150);
    drag('dragend', tabsIn(1)[0] as HTMLElement);
    await eventually(() => expect(picture()).toEqual([['Draft method section(preview)*'], ['Codex rollout parser*']]));

    // Back into pane 1's tab strip (at its end): pane 2 goes.
    drag('dragstart', tabsIn(2)[0] as HTMLElement);
    const strip = screen.getByRole('tablist', { name: 'Tabs in pane 1' });
    drag('dragover', strip, 1_000, 5);
    drag('drop', strip, 1_000, 5);
    drag('dragend', tabsIn(1)[1] as HTMLElement);
    await eventually(() => expect(picture()).toEqual([['Draft method section(preview)', 'Codex rollout parser*']]));

    // A drag from anywhere else (a file from the desktop) is not taken.
    expect(drag('dragover', strip).defaultPrevented).toBe(false);
    expect(drag('dragover', within(pane(1)).getByRole('tabpanel')).defaultPrevented).toBe(false);
  }, 20_000);
});
