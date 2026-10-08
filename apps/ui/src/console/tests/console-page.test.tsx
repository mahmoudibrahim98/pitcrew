// @vitest-environment happy-dom
// @vitest-environment-options {"url": "http://localhost:5173/"}

// The console as the app mounts it: the shell's router with the console feature, against the mock
// hub. Routes, the filters in the URL, the palette's commands, the keys between panes, the narrow
// layout, the remembered pane sizes, and the Chat | Terminal switch.

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
import { COMPACT_BELOW, initialPanes, NARROW_BELOW, PANE_WIDTH, usePanes } from '../panes.ts';
import { clearDrafts } from '../workbench/drafts.ts';
import { resetWorkbenchStores } from '../workbench/store.ts';
import { eventually, ID, renderWithHub, startHub, stubLayout, unmountAndSettle, type HubProcess } from './harness.tsx';

const WS = '01JB000000000000000WSP0001';
const UNKNOWN = '01JB000000000000000SES9999';

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
  localStorage.clear();
  // The workbench keeps one layout per workspace for the page's life; each test starts afresh.
  resetWorkbenchStores();
  clearDrafts();
  usePanes.setState(initialPanes);
  useShell.setState(initialShellState);
});

function renderConsole(path: string) {
  const router = createAppRouter([feature], { history: createMemoryHistory({ initialEntries: [path] }) });
  // In Strict Mode at the root, as src/main.tsx mounts the app: effects run, are undone, and run
  // again.
  const { requests } = renderWithHub(hub, <RouterProvider router={router} />, { strict: true });
  /** A palette command's context, as the shell's palette builds it. */
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
  const location = () => router.state.location;
  return { router, run, location, requests };
}

const rowOf = (id: string) => document.querySelector<HTMLElement>(`[data-session="${id}"]`);
const listedSessions = () =>
  [...document.querySelectorAll<HTMLElement>('[role="option"][data-session]')].map((o) => o.dataset.session);
const pane = (name: string) => document.querySelector<HTMLElement>(`[data-pane="${name}"]`);
const focusedPane = () => document.activeElement?.closest<HTMLElement>('[data-pane]')?.dataset.pane;

describe('the Agent console', () => {
  it('opens on the list, shows a chosen session, and links to its task', async () => {
    const { location } = renderConsole(`/w/${WS}/console`);
    await screen.findByRole('heading', { level: 1, name: 'Agent console' }, { timeout: 8_000 });
    await screen.findByRole('listbox', { name: 'Sessions' });
    await eventually(() => expect(listedSessions()).toHaveLength(6));
    expect(screen.getByText('Choose a session')).toBeTruthy();
    expect(screen.getByRole('group', { name: 'Session filters' })).toBeTruthy();

    fireEvent.click(rowOf(ID.ses1) as HTMLElement);
    await eventually(() => expect(location().pathname).toBe(paths.session(WS, ID.ses1)));
    await screen.findByRole('heading', { level: 2, name: 'Draft method section' });
    expect(rowOf(ID.ses1)?.getAttribute('aria-selected')).toBe('true');
    // The whole transcript, from its first item.
    const transcript = await screen.findByRole('group', { name: 'Transcript' });
    await eventually(() => expect(transcript.textContent).toContain('Go ahead with §3.2'));
    expect(screen.getByRole('textbox', { name: /message/i })).toBeTruthy();

    const task = await screen.findByRole('link', { name: /^PAP-1 · / });
    expect(task.getAttribute('href')).toBe(paths.task(WS, 'PAP-1'));
    // The workstream comes from a query of its own, which may land after the task's.
    const workstream = await screen.findByRole('link', { name: 'Submission' });
    expect(workstream.getAttribute('href')).toMatch(/\/projects\/[^/]+\/workstreams\/[^/]+$/);
    fireEvent.click(task);
    await eventually(() => expect(location().pathname).toBe(paths.task(WS, 'PAP-1')));
    await screen.findByRole('heading', { level: 1, name: /^PAP-1/ });
  }, 20_000);

  it('keeps the filters in the URL', async () => {
    const { location } = renderConsole(`/w/${WS}/console?state=waiting`);
    await screen.findByRole('listbox', { name: 'Sessions' }, { timeout: 8_000 });
    await eventually(() => expect(listedSessions()).toEqual([ID.ses3]));
    const filters = screen.getByRole('group', { name: 'Session filters' });
    expect((within(filters).getByRole('checkbox', { name: /^Waiting/ }) as HTMLInputElement).checked).toBe(true);
    expect(screen.getByTestId('session-count').textContent).toBe('1 of 6 sessions');

    // Each change waits to be on screen before the next, as a person's would.
    const laptop = (await within(filters).findByRole('checkbox', { name: /^This laptop/ })) as HTMLInputElement;
    fireEvent.click(laptop);
    await eventually(() => expect(laptop.checked).toBe(true));
    await eventually(() => expect(location().search).toEqual({ machine: ID.laptop, state: 'waiting' }));
    expect(location().href).toContain(`machine=${ID.laptop}&state=waiting`);

    const working = within(filters).getByRole('checkbox', { name: /^Working/ }) as HTMLInputElement;
    fireEvent.click(working);
    await eventually(() => expect(working.checked).toBe(true));
    fireEvent.click(within(filters).getByRole('checkbox', { name: /^Waiting/ }));
    await eventually(() => expect(location().search).toEqual({ machine: ID.laptop, state: 'working' }));
    await eventually(() => expect(listedSessions()).toEqual([ID.ses1]));

    // Clearing keeps focus in the filters, though the Clear button goes away.
    fireEvent.click(within(filters).getByRole('button', { name: 'Clear' }));
    await eventually(() => expect(location().search).toEqual({}));
    await eventually(() => expect(within(filters).queryByRole('button', { name: 'Clear' })).toBeNull());
    expect(focusedPane()).toBe('filters');
    await eventually(() => expect(listedSessions()).toHaveLength(6));
  }, 20_000);

  it("runs the palette's commands from anywhere", async () => {
    const { run, location } = renderConsole(`/w/${WS}/inbox`);
    await screen.findByRole('heading', { level: 1, name: 'Inbox' }, { timeout: 8_000 });

    run('console-waiting');
    await eventually(() => expect(location().pathname).toBe(paths.console(WS)));
    await eventually(() => expect(location().search).toEqual({ state: 'waiting' }));
    await eventually(() => expect(listedSessions()).toEqual([ID.ses3]));
    await eventually(() => expect(focusedPane()).toBe('list'));

    run('console-filter-machine');
    await eventually(() => expect(document.activeElement?.closest('[data-facet="machine"]')).not.toBeNull());

    run('console-clear-filters');
    await eventually(() => expect(location().search).toEqual({}));
    await eventually(() => expect(listedSessions()).toHaveLength(6));

    // From a session, "Jump to a session…" goes to the list and keeps the session beside it.
    fireEvent.click(rowOf(ID.ses2) as HTMLElement);
    await eventually(() => expect(location().pathname).toBe(paths.session(WS, ID.ses2)));
    (pane('filters')?.querySelector('input') as HTMLElement).focus();
    run('console-jump');
    await eventually(() => expect(document.activeElement?.getAttribute('role')).toBe('listbox'));
    expect(location().pathname).toBe(paths.session(WS, ID.ses2));
  }, 20_000);

  it('moves between panes with F6; the arrows choose the session, Enter goes to the composer', async () => {
    const { location } = renderConsole(paths.session(WS, ID.ses4));
    const composer = (await screen.findByRole('textbox', { name: /message/i }, { timeout: 8_000 })) as HTMLTextAreaElement;
    await eventually(() => expect(composer.disabled).toBe(false));
    await eventually(() => expect(rowOf(ID.ses4)).not.toBeNull());

    const checkbox = pane('filters')?.querySelector('input') as HTMLElement;
    checkbox.focus();
    fireEvent.keyDown(checkbox, { key: 'F6' });
    const listbox = screen.getByRole('listbox', { name: 'Sessions' });
    expect(document.activeElement).toBe(listbox);
    // The list starts from the session on screen.
    expect(listbox.getAttribute('aria-activedescendant')).toContain(ID.ses4);

    fireEvent.keyDown(listbox, { key: 'ArrowUp' });
    expect(listbox.getAttribute('aria-activedescendant')).toContain(ID.ses3);
    await eventually(() => expect(location().pathname).toBe(paths.session(WS, ID.ses3)));
    await screen.findByRole('heading', { level: 2, name: 'Review parser benchmarks' });
    // Focus stays in the list while the arrows choose.
    expect(document.activeElement).toBe(listbox);

    fireEvent.keyDown(listbox, { key: 'Enter' });
    await eventually(() => expect(focusedPane()).toBe('composer'));
    const active = () => document.activeElement as HTMLElement;
    fireEvent.keyDown(active(), { key: 'F6' });
    expect(focusedPane()).toBe('filters');
    fireEvent.keyDown(active(), { key: 'F6', shiftKey: true });
    expect(focusedPane()).toBe('composer');
    fireEvent.keyDown(active(), { key: 'F6', shiftKey: true });
    expect(focusedPane()).toBe('chat');
    expect(active().getAttribute('aria-label')).toBe('Transcript');
    // Ctrl F6 and friends are left alone.
    fireEvent.keyDown(active(), { key: 'F6', ctrlKey: true });
    expect(focusedPane()).toBe('chat');
  }, 20_000);

  it('shows one pane at a time when narrow, with a way back', async () => {
    const saved = Object.getOwnPropertyDescriptor(HTMLElement.prototype, 'clientWidth');
    Object.defineProperty(HTMLElement.prototype, 'clientWidth', {
      configurable: true,
      get(this: HTMLElement) {
        return this.hasAttribute('data-console-layout') ? NARROW_BELOW - 120 : 0;
      },
    });
    try {
      const { location } = renderConsole(`/w/${WS}/console`);
      await screen.findByRole('listbox', { name: 'Sessions' }, { timeout: 8_000 });
      expect(document.querySelector('[data-console-layout]')?.getAttribute('data-console-layout')).toBe('narrow');
      expect(screen.queryByRole('group', { name: 'Session filters' })).toBeNull();
      expect(screen.queryByText('Choose a session')).toBeNull();

      fireEvent.click(screen.getByRole('button', { name: 'Filters' }));
      await screen.findByRole('group', { name: 'Session filters' });
      expect(screen.queryByRole('listbox', { name: 'Sessions' })).toBeNull();
      fireEvent.click(screen.getByRole('button', { name: 'Sessions' }));
      await screen.findByRole('listbox', { name: 'Sessions' });

      await eventually(() => expect(rowOf(ID.ses1)).not.toBeNull());
      fireEvent.click(rowOf(ID.ses1) as HTMLElement);
      await screen.findByRole('heading', { level: 2, name: 'Draft method section' });
      expect(screen.queryByRole('listbox', { name: 'Sessions' })).toBeNull();
      fireEvent.click(screen.getByRole('button', { name: 'Sessions' }));
      await eventually(() => expect(location().pathname).toBe(paths.console(WS)));
      await screen.findByRole('listbox', { name: 'Sessions' });
    } finally {
      if (saved === undefined) Reflect.deleteProperty(HTMLElement.prototype, 'clientWidth');
      else Object.defineProperty(HTMLElement.prototype, 'clientWidth', saved);
    }
  }, 20_000);

  it('folds the filters away in a compact console, without changing the remembered choice', async () => {
    const saved = Object.getOwnPropertyDescriptor(HTMLElement.prototype, 'clientWidth');
    let width = COMPACT_BELOW - 40;
    Object.defineProperty(HTMLElement.prototype, 'clientWidth', {
      configurable: true,
      get(this: HTMLElement) {
        return this.hasAttribute('data-console-layout') ? width : 0;
      },
    });
    const filtersGroup = () => screen.queryByRole('group', { name: 'Session filters' });
    try {
      renderConsole(`/w/${WS}/console`);
      await screen.findByRole('listbox', { name: 'Sessions' }, { timeout: 8_000 });
      expect(document.querySelector('[data-console-layout]')?.getAttribute('data-console-layout')).toBe('wide');
      const toggle = screen.getByRole('button', { name: 'Filters' });
      expect(toggle.getAttribute('aria-pressed')).toBe('false');
      expect(filtersGroup()).toBeNull();
      expect(usePanes.getState().filtersOpen).toBe(true);

      // Opened and closed here: shown while open, and the remembered choice stays as it was.
      fireEvent.click(toggle);
      await screen.findByRole('group', { name: 'Session filters' });
      expect(toggle.getAttribute('aria-pressed')).toBe('true');
      fireEvent.click(toggle);
      expect(filtersGroup()).toBeNull();
      expect(usePanes.getState().filtersOpen).toBe(true);
      expect(localStorage.getItem('pitcrew.console') ?? '').not.toContain('"filtersOpen":false');

      // A wide console shows them again, as remembered.
      await unmountAndSettle();
      width = COMPACT_BELOW + 400;
      renderConsole(`/w/${WS}/console`);
      await screen.findByRole('group', { name: 'Session filters' }, { timeout: 8_000 });
      expect(screen.getByRole('button', { name: 'Filters' }).getAttribute('aria-pressed')).toBe('true');
    } finally {
      if (saved === undefined) Reflect.deleteProperty(HTMLElement.prototype, 'clientWidth');
      else Object.defineProperty(HTMLElement.prototype, 'clientWidth', saved);
    }
  }, 30_000);

  it('remembers the pane sizes and whether the filters show', async () => {
    renderConsole(`/w/${WS}/console`);
    const edge = await screen.findByRole('separator', { name: 'Resize Sessions' }, { timeout: 8_000 });
    expect(edge.getAttribute('aria-valuenow')).toBe(String(PANE_WIDTH.list.initial));
    fireEvent.keyDown(edge, { key: 'ArrowRight' });
    expect(edge.getAttribute('aria-valuenow')).toBe(String(PANE_WIDTH.list.initial + 16));
    fireEvent.keyDown(edge, { key: 'End' });
    expect(edge.getAttribute('aria-valuenow')).toBe(String(PANE_WIDTH.list.max));

    const toggle = screen.getByRole('button', { name: 'Filters' });
    expect(toggle.getAttribute('aria-pressed')).toBe('true');
    fireEvent.click(toggle);
    expect(toggle.getAttribute('aria-pressed')).toBe('false');
    expect(screen.queryByRole('group', { name: 'Session filters' })).toBeNull();

    const stored = JSON.parse(localStorage.getItem('pitcrew.console') ?? '{}') as { state?: unknown };
    expect(stored.state).toEqual({
      filtersOpen: false,
      filtersWidth: PANE_WIDTH.filters.initial,
      listWidth: PANE_WIDTH.list.max,
    });
  }, 20_000);

  it('keeps Chat or Terminal in the URL; F6 reaches the terminal; without one the switch says why', async () => {
    const { location, router } = renderConsole(`${paths.session(WS, ID.ses6)}?view=terminal`);
    await screen.findByRole('heading', { level: 2, name: 'Co-author responses' }, { timeout: 8_000 });
    const option = await screen.findByRole('radio', { name: 'Terminal' });
    await eventually(() => expect(option.getAttribute('aria-disabled')).toBe('true'));
    expect(screen.getByTestId('terminal-unavailable').textContent).toBe('This session has no terminal.');
    expect(option.getAttribute('aria-describedby')).toBe(screen.getByTestId('terminal-unavailable').id);
    // The link asked for the terminal; there is none, so the chat shows, and choosing it does nothing.
    await screen.findByRole('group', { name: 'Transcript' });
    fireEvent.click(option);
    expect(location().search).toEqual({ view: 'terminal' });
    expect(screen.queryByRole('group', { name: 'Terminal' })).toBeNull();

    // Another session keeps the view; SES0001 has a terminal, so it shows.
    await eventually(() => expect(rowOf(ID.ses1)).not.toBeNull());
    fireEvent.click(rowOf(ID.ses1) as HTMLElement);
    await eventually(() => expect(location().pathname).toBe(paths.session(WS, ID.ses1)));
    expect(location().search).toEqual({ view: 'terminal' });
    const terminal = await screen.findByRole('group', { name: 'Terminal' });
    expect(pane('composer')).toBeNull();
    expect(screen.getByRole('radio', { name: 'Terminal' }).getAttribute('aria-checked')).toBe('true');

    // F6 goes from the list to the terminal.
    const listbox = screen.getByRole('listbox', { name: 'Sessions' });
    listbox.focus();
    fireEvent.keyDown(listbox, { key: 'F6' });
    expect(document.activeElement).toBe(terminal);
    expect(focusedPane()).toBe('terminal');
    // A terminal in control mode keeps F6 for its program.
    terminal.setAttribute('data-shell-keys', 'none');
    fireEvent.keyDown(terminal, { key: 'F6' });
    expect(document.activeElement).toBe(terminal);
    terminal.removeAttribute('data-shell-keys');

    // Chat is no parameter at all, and replaces the history entry rather than adding one.
    const entries = router.history.length;
    fireEvent.click(screen.getByRole('radio', { name: 'Chat' }));
    await eventually(() => expect(location().search).toEqual({}));
    expect(router.history.length).toBe(entries);
    await screen.findByRole('group', { name: 'Transcript' });
    expect(screen.queryByRole('group', { name: 'Terminal' })).toBeNull();
    fireEvent.click(screen.getByRole('radio', { name: 'Terminal' }));
    await eventually(() => expect(location().search).toEqual({ view: 'terminal' }));
    await screen.findByRole('group', { name: 'Terminal' });
  }, 20_000);

  it("shows the session's work as a third view, kept in the URL", async () => {
    const { location } = renderConsole(`${paths.session(WS, ID.ses1)}?view=work`);
    await screen.findByRole('heading', { level: 2, name: 'Draft method section' }, { timeout: 8_000 });
    const option = await screen.findByRole('radio', { name: 'Work' });
    await eventually(() => expect(option.getAttribute('aria-checked')).toBe('true'));
    // The chat and composer are gone while Work shows, as they are for the terminal.
    expect(screen.queryByRole('group', { name: 'Transcript' })).toBeNull();
    expect(pane('composer')).toBeNull();

    const work = await screen.findByRole('region', { name: 'Work' });
    await eventually(() =>
      expect([...work.querySelectorAll('[data-summary]')].map((node) => node.textContent)).toEqual([
        '@writer edited method.tex (+84 −12)',
        '@sam dispatched @writer to PAP-1, @writer moved PAP-1 to in progress, updated the plan for PAP-1 (2 of 4 done)',
      ]),
    );
    expect(within(work).getByText('1 file touched (+84 −12)')).toBeTruthy();

    // F6 from the list reaches it, as it does the terminal.
    const listbox = screen.getByRole('listbox', { name: 'Sessions' });
    listbox.focus();
    fireEvent.keyDown(listbox, { key: 'F6' });
    expect(focusedPane()).toBe('work');

    // Chat is the default again; the work view keeps no parameter once left.
    fireEvent.click(screen.getByRole('radio', { name: 'Chat' }));
    await eventually(() => expect(location().search).toEqual({}));
    await screen.findByRole('group', { name: 'Transcript' });
    expect(screen.queryByRole('region', { name: 'Work' })).toBeNull();

    // Choosing Work again puts it back in the URL.
    fireEvent.click(screen.getByRole('radio', { name: 'Work' }));
    await eventually(() => expect(location().search).toEqual({ view: 'work' }));
    await screen.findByRole('region', { name: 'Work' });
  }, 20_000);

  it('says so when the session does not exist', async () => {
    const { requests } = renderConsole(paths.session(WS, UNKNOWN));
    // Wait for the 404 itself first (the stream's `hello` can be slow on a busy machine, and that
    // wait would otherwise be folded into, and indistinguishable from, the text's own timeout
    // below).
    await eventually(
      () => expect(requests.some((r) => r.path === `/v1/sessions/${UNKNOWN}` && r.status === 404)).toBe(true),
      { timeout: 8_000 },
    );
    await screen.findByText('No such session', undefined, { timeout: 8_000 });
    expect(screen.getByRole('listbox', { name: 'Sessions' })).toBeTruthy();
  }, 20_000);
});
