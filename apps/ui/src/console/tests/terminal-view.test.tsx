// @vitest-environment happy-dom
// @vitest-environment-options {"url": "http://localhost:5173/"}

// TerminalView with xterm.js replaced by a fake (happy-dom has no canvas to draw on): the options
// that keep hostile output in its place, view and control modes, the release chord, the end,
// truncation, refused input, the renderer fallback, theme changes, the screen reader setting,
// links, and that unmounting releases everything. The real xterm runs in the Playwright specs.

import { act, fireEvent, screen } from '@testing-library/react';
import type { ReactNode } from 'react';
import { afterAll, afterEach, beforeAll, beforeEach, describe, expect, it, vi } from 'vitest';
import { keys, type Machine } from '../../data/index.ts';
import { OpenExternalProvider } from '../render/links.tsx';
import { HIGH_WATER, LOW_WATER } from '../terminal/controller.ts';
import { MAX_LINK_LENGTH, SCROLLBACK } from '../terminal/options.ts';
import { useTerminalPrefs } from '../terminal/prefs.ts';
import { INPUT_LIMIT, SEND_LIMIT } from '../terminal/socket.ts';
import { TerminalView } from '../terminal/terminal-view.tsx';
import { eventually, ID, renderWithHub, startHub, unmountAndSettle, type HubProcess } from './harness.tsx';
import { FakeEnvironment, fakeSockets } from './terminal-fakes.ts';

const fakes = vi.hoisted(() => {
  type Listener<T> = (value: T) => void;
  class Emitter<T> {
    readonly listeners = new Set<Listener<T>>();
    on = (listener: Listener<T>) => {
      this.listeners.add(listener);
      return { dispose: () => this.listeners.delete(listener) };
    };
    fire(value: T) {
      for (const listener of this.listeners) listener(value);
    }
  }
  interface Addon {
    activate(terminal: unknown): void;
    dispose(): void;
  }

  class FakeTerminal {
    static instances: FakeTerminal[] = [];
    options: Record<string, unknown>;
    cols = 80;
    rows = 24;
    element: HTMLElement | undefined;
    textarea: HTMLTextAreaElement | undefined;
    written: Uint8Array[] = [];
    readonly osc = new Map<number, (data: string) => boolean>();
    keyHandler: ((event: KeyboardEvent) => boolean) | undefined;
    readonly data = new Emitter<string>();
    readonly binary = new Emitter<string>();
    readonly resized = new Emitter<{ cols: number; rows: number }>();
    readonly addons: Addon[] = [];
    disposed = false;
    readonly parser = {
      registerOscHandler: (code: number, handler: (data: string) => boolean) => {
        this.osc.set(code, handler);
        return { dispose: () => this.osc.delete(code) };
      },
    };
    onData = this.data.on;
    onBinary = this.binary.on;
    onResize = this.resized.on;

    constructor(options: Record<string, unknown>) {
      this.options = { ...options };
      FakeTerminal.instances.push(this);
    }

    open(parent: HTMLElement) {
      this.element = document.createElement('div');
      this.element.className = 'xterm';
      this.textarea = document.createElement('textarea');
      this.element.append(this.textarea);
      parent.append(this.element);
    }

    loadAddon(addon: Addon) {
      this.addons.push(addon);
      addon.activate(this);
    }

    attachCustomKeyEventHandler(handler: (event: KeyboardEvent) => boolean) {
      this.keyHandler = handler;
    }

    /** With `deferWrites`, xterm parses only when the test says so (`parse(n)`). */
    static deferWrites = false;
    readonly parsing: (() => void)[] = [];

    write(data: Uint8Array, callback?: () => void) {
      this.written.push(data);
      if (!FakeTerminal.deferWrites) callback?.();
      else if (callback !== undefined) this.parsing.push(callback);
    }

    /** Finishes parsing the next `count` writes, in order. */
    parse(count: number) {
      for (const done of this.parsing.splice(0, count)) done();
    }

    focus() {
      this.textarea?.focus();
    }

    hasSelection() {
      return false;
    }

    getSelection() {
      return '';
    }

    scrollLines() {}
    scrollPages() {}
    scrollToTop() {}
    scrollToBottom() {}

    dispose() {
      this.disposed = true;
      for (const addon of [...this.addons]) addon.dispose();
      this.element?.remove();
    }

    /** What xterm does with typing: nothing reaches `onData` while stdin is disabled. */
    type(text: string) {
      if (this.options.disableStdin !== true) this.data.fire(text);
    }

    /** Asks the key handler about a key, as xterm does before acting on it. */
    key(init: KeyboardEventInit, type = 'keydown') {
      return this.keyHandler?.(new KeyboardEvent(type, { cancelable: true, ...init }));
    }

    get text() {
      return this.written.map((bytes) => new TextDecoder().decode(bytes)).join('');
    }
  }

  class FakeFit {
    fits = 0;
    activate() {}
    dispose() {}
    fit() {
      this.fits += 1;
    }
  }

  class FakeWebgl {
    static fail = false;
    static instances: FakeWebgl[] = [];
    readonly contextLoss = new Emitter<void>();
    onContextLoss = this.contextLoss.on;
    disposed = false;
    constructor() {
      FakeWebgl.instances.push(this);
    }
    activate() {
      if (FakeWebgl.fail) throw new Error('WebGL2 not supported');
    }
    dispose() {
      this.disposed = true;
    }
  }

  return { FakeTerminal, FakeFit, FakeWebgl };
});

vi.mock('@xterm/xterm', () => ({ Terminal: fakes.FakeTerminal }));
vi.mock('@xterm/addon-fit', () => ({ FitAddon: fakes.FakeFit }));
vi.mock('@xterm/addon-webgl', () => ({ WebglAddon: fakes.FakeWebgl }));
vi.mock('@xterm/xterm/css/xterm.css', () => ({}));

type Term = InstanceType<typeof fakes.FakeTerminal>;

let hub: HubProcess;

beforeAll(async () => {
  hub = await startHub();
});

afterAll(async () => {
  await hub.close();
});

beforeEach(() => {
  fakes.FakeTerminal.instances = [];
  fakes.FakeTerminal.deferWrites = false;
  fakes.FakeWebgl.instances = [];
  fakes.FakeWebgl.fail = false;
});

afterEach(async () => {
  await unmountAndSettle();
  localStorage.clear();
  useTerminalPrefs.setState({ screenReader: false });
  document.documentElement.removeAttribute('data-theme');
});

const frame = () => screen.getByRole('group', { name: 'Terminal' });
const mode = () => screen.getByTestId('terminal-mode').textContent;
const status = () => screen.getByTestId('terminal-status').textContent;

/** Renders the view with fake sockets, waits for xterm to open and the socket to connect. */
async function renderTerminal(
  options: { open?: (url: string) => void; fetch?: typeof fetch; strict?: boolean } = {},
) {
  const sockets = fakeSockets();
  const environment = new FakeEnvironment();
  let view: ReactNode = (
    <TerminalView
      sessionId={ID.ses1}
      socket={sockets.factory}
      socketOptions={{ environment, random: () => 0.5, backoff: { initialMs: 10, maxMs: 20 }, resizeMs: 10 }}
    />
  );
  if (options.open !== undefined) view = <OpenExternalProvider open={options.open}>{view}</OpenExternalProvider>;
  const result = renderWithHub(hub, view, {
    ...(options.fetch === undefined ? {} : { fetch: options.fetch }),
    strict: options.strict === true,
  });
  const term = await eventually(() => {
    const found = fakes.FakeTerminal.instances.at(-1);
    expect(found?.element).toBeDefined();
    return found as Term;
  });
  await eventually(() => expect(sockets.sockets.length).toBeGreaterThan(0));
  await eventually(() => expect(frame().dataset.renderer).toBeDefined());
  return { ...result, term, sockets, environment };
}

async function live() {
  const rendered = await renderTerminal();
  act(() => rendered.sockets.last().open());
  await eventually(() => expect(status()).toBe('Live'));
  return rendered;
}

describe('TerminalView', () => {
  it('starts xterm with options that keep hostile output in its place', async () => {
    const { term } = await renderTerminal();
    expect(term.options).toMatchObject({
      allowProposedApi: false,
      allowTransparency: false,
      disableStdin: true,
      scrollback: SCROLLBACK,
      windowOptions: {},
      screenReaderMode: false,
      minimumContrastRatio: 4.5,
    });
    expect(SCROLLBACK).toBe(5_000);
    expect((term.options.linkHandler as { allowNonHttpProtocols?: boolean }).allowNonHttpProtocols).toBe(false);
    // Titles and the clipboard: swallowed before xterm sees them (8 is the links' length check).
    expect([...term.osc.keys()].sort((a, b) => a - b)).toEqual([0, 1, 2, 8, 52]);
    for (const code of [0, 1, 2, 52]) expect(term.osc.get(code)?.('x;y')).toBe(true);
    // Only the fit and WebGL addons: no clipboard, links or image addon.
    expect(term.addons.map((addon) => addon.constructor.name).sort()).toEqual(['FakeFit', 'FakeWebgl']);
    expect(frame().dataset.renderer).toBe('webgl');
    // xterm's input is never a tab stop of its own; the frame is.
    expect(term.textarea?.tabIndex).toBe(-1);
    expect(frame().tabIndex).toBe(0);
  });

  it('connects with the fitted size and writes the output it receives', async () => {
    const { term, sockets } = await live();
    expect(sockets.last().path).toBe(`/v1/sessions/${ID.ses1}/terminal?cols=80&rows=24&from=0`);
    act(() => sockets.last().output('\x1b[1mhello\x1b[0m'));
    expect(term.text).toBe('\x1b[1mhello\x1b[0m');
    // xterm's own resize (a fit) reaches the hub, settled.
    act(() => term.resized.fire({ cols: 100, rows: 30 }));
    await eventually(() => expect(sockets.last().controls).toEqual([{ type: 'resize', cols: 100, rows: 30 }]));
  });

  it('sends nothing in view mode; Enter takes control and keys go out as binary', async () => {
    const { term, sockets } = await live();
    expect(mode()).toBe('Viewing');
    expect(frame().hasAttribute('data-shell-keys')).toBe(false);
    term.type('ignored');
    expect(sockets.last().sent).toEqual([]);
    // In view mode xterm acts on no key at all, so the shell keeps them.
    expect(term.key({ key: 'k', code: 'KeyK', ctrlKey: true })).toBe(false);

    act(() => frame().focus());
    fireEvent.keyDown(frame(), { key: 'Enter', code: 'Enter' });
    expect(mode()).toBe('In control');
    expect(frame().getAttribute('data-shell-keys')).toBe('none');
    expect(term.options.disableStdin).toBe(false);
    expect(document.activeElement).toBe(term.textarea);
    act(() => term.type('ls\r'));
    expect(sockets.last().keys).toEqual([...new TextEncoder().encode('ls\r')]);
    // Every key but the chord is xterm's: Esc, Tab, F6, Ctrl K.
    for (const init of [{ key: 'Escape' }, { key: 'Tab' }, { key: 'F6' }, { key: 'k', ctrlKey: true }]) {
      expect(term.key({ code: '', ...init })).toBe(true);
    }
    expect(screen.getByText(/keys go to the program/)).toBeTruthy();
  });

  it('Ctrl+Shift+X releases: the program never sees it, and focus and the keys go back', async () => {
    const { term, sockets } = await live();
    act(() => frame().focus());
    fireEvent.keyDown(frame(), { key: 'Enter', code: 'Enter' });
    expect(mode()).toBe('In control');
    let handled: boolean | undefined;
    act(() => {
      handled = term.key({ key: 'X', code: 'KeyX', ctrlKey: true, shiftKey: true });
    });
    expect(handled).toBe(false);
    expect(mode()).toBe('Viewing');
    expect(frame().hasAttribute('data-shell-keys')).toBe(false);
    expect(term.options.disableStdin).toBe(true);
    expect(document.activeElement).toBe(frame());
    expect(sockets.last().sent).toEqual([]);
    // A Cyrillic layout: the key's position.
    fireEvent.keyDown(frame(), { key: 'Enter', code: 'Enter' });
    act(() => {
      handled = term.key({ key: 'Ч', code: 'KeyX', ctrlKey: true, shiftKey: true });
    });
    expect(handled).toBe(false);
    expect(mode()).toBe('Viewing');
  });

  it('the Take control and Release buttons, and the announcement', async () => {
    const { term } = await live();
    const announcer = () => document.querySelector('[aria-live="polite"]')?.textContent;
    expect(announcer()).toMatch(/^Viewing the terminal: input is off/);
    fireEvent.click(screen.getByRole('button', { name: 'Take control' }));
    expect(mode()).toBe('In control');
    expect(announcer()).toMatch(/^You have control of the terminal/);
    expect(document.activeElement).toBe(term.textarea);
    const release = screen.getByRole('button', { name: 'Release' });
    expect(release.getAttribute('aria-keyshortcuts')).toBe('Control+Shift+X');
    fireEvent.click(release);
    expect(mode()).toBe('Viewing');
    expect(document.activeElement).toBe(frame());
  });

  it('in view mode, focus that lands in xterm goes back to the frame', async () => {
    const { term } = await live();
    act(() => term.textarea?.focus());
    expect(document.activeElement).toBe(frame());
  });

  it('shows the truncated marker', async () => {
    const { sockets } = await live();
    expect(screen.queryByTestId('terminal-truncated')).toBeNull();
    act(() => sockets.last().text({ type: 'truncated', from: 4_194_304 }));
    expect(screen.getByTestId('terminal-truncated').textContent).toBe('Earlier output is no longer available.');
  });

  it('when the program ends: says so, gives control back, and takes no input', async () => {
    const { term, sockets } = await live();
    fireEvent.click(screen.getByRole('button', { name: 'Take control' }));
    act(() => sockets.last().text({ type: 'exit' }));
    expect(status()).toBe('The program ended.');
    expect(mode()).toBe('Viewing');
    expect(term.options.disableStdin).toBe(true);
    expect((screen.getByRole('button', { name: 'Take control' }) as HTMLButtonElement).disabled).toBe(true);
    fireEvent.keyDown(frame(), { key: 'Enter', code: 'Enter' });
    expect(mode()).toBe('Viewing');
  });

  it('says when keystrokes wait for the connection, and when they are refused', async () => {
    const { term, sockets } = await live();
    fireEvent.click(screen.getByRole('button', { name: 'Take control' }));
    act(() => sockets.last().drop(1006));
    act(() => term.type('queued'));
    expect(screen.getByRole('alert').textContent).toMatch(/sent when the connection is back/);
    act(() => term.type('x'.repeat(INPUT_LIMIT)));
    expect(screen.getByRole('alert').textContent).toMatch(/at most 64 KiB/);
    // Back: the waiting keys go out, and the message goes.
    await eventually(() => expect(sockets.sockets.length).toBe(2));
    act(() => sockets.last().open());
    expect(sockets.last().keys).toEqual([...new TextEncoder().encode('queued')]);
    expect(screen.queryByRole('alert')).toBeNull();
  });

  it('falls back to the DOM renderer without WebGL', async () => {
    fakes.FakeWebgl.fail = true;
    const { term } = await renderTerminal();
    await eventually(() => expect(frame().dataset.renderer).toBe('dom'));
    expect(term.addons.map((addon) => addon.constructor.name)).toEqual(['FakeFit', 'FakeWebgl']);
    expect(fakes.FakeWebgl.instances[0]?.disposed).toBe(true);
  });

  it('falls back to the DOM renderer when the WebGL context is lost', async () => {
    await renderTerminal();
    expect(frame().dataset.renderer).toBe('webgl');
    const webgl = fakes.FakeWebgl.instances[0];
    act(() => webgl?.contextLoss.fire());
    expect(frame().dataset.renderer).toBe('dom');
    expect(webgl?.disposed).toBe(true);
  });

  it('follows theme changes', async () => {
    const { term } = await renderTerminal();
    const before = term.options.theme;
    document.documentElement.setAttribute('data-theme', 'dark');
    await eventually(() => expect(term.options.theme).not.toBe(before));
  });

  it('remembers the screen reader mode in this browser', async () => {
    const { term } = await renderTerminal();
    fireEvent.click(screen.getByRole('checkbox', { name: 'Screen reader mode' }));
    expect(term.options.screenReaderMode).toBe(true);
    expect(JSON.parse(localStorage.getItem('pitcrew.terminal') ?? '{}')).toMatchObject({ state: { screenReader: true } });
  });

  it('opens only http and https links, on a modifier-click, through the console opener', async () => {
    const open = vi.fn();
    const { term } = await renderTerminal({ open });
    const links = term.options.linkHandler as {
      activate(event: MouseEvent, text: string): void;
      hover?(event: MouseEvent, text: string): void;
      leave?(event: MouseEvent, text: string): void;
    };
    const click = (init: MouseEventInit = {}) => new MouseEvent('click', { cancelable: true, ...init });
    links.activate(click(), 'https://example.test/docs');
    expect(open).not.toHaveBeenCalled();
    links.activate(click({ ctrlKey: true }), 'javascript:alert(1)');
    links.activate(click({ ctrlKey: true }), 'file:///etc/passwd');
    links.activate(click({ metaKey: true }), 'not a url');
    expect(open).not.toHaveBeenCalled();
    links.activate(click({ ctrlKey: true }), 'https://example.test/docs');
    links.activate(click({ metaKey: true }), 'http://127.0.0.1:8080/x');
    expect(open.mock.calls).toEqual([['https://example.test/docs'], ['http://127.0.0.1:8080/x']]);
    act(() => links.hover?.(click(), 'https://example.test/docs'));
    expect(frame().getAttribute('title')).toBe('Ctrl+click to open https://example.test/docs');
    act(() => links.leave?.(click(), 'https://example.test/docs'));
    expect(frame().hasAttribute('title')).toBe(false);
  });

  it('holds the socket past 4 MiB not yet parsed, and resumes at 512 KiB from the bytes received', async () => {
    fakes.FakeTerminal.deferWrites = true;
    const { term, sockets } = await live();
    const half = new Uint8Array(512 * 1024);
    // 4 MiB waiting to be parsed is not over the mark…
    for (let i = 0; i < HIGH_WATER / half.byteLength; i += 1) act(() => sockets.last().output(half));
    expect(sockets.last().closedWith).toBeUndefined();
    // …one more chunk is: the socket closes, and the hub keeps the rest.
    act(() => sockets.last().output(half));
    const first = sockets.sockets[0];
    expect(first?.closedWith?.code).toBe(1000);
    expect(status()).toBe('Catching up with the output…');
    // Output that was already on its way is not taken.
    act(() => first?.output('late'));
    expect(term.written).toHaveLength(9);

    // Parsed down to 1 MiB: still held.
    act(() => term.parse(7));
    expect(sockets.sockets).toHaveLength(1);
    // Down to 512 KiB: it resumes from the bytes received.
    act(() => term.parse(1));
    expect(term.parsing).toHaveLength(1);
    expect(9 * half.byteLength - 8 * half.byteLength).toBe(LOW_WATER);
    expect(sockets.sockets).toHaveLength(2);
    expect(sockets.last().from).toBe(9 * half.byteLength);
    // Catching up is not a dropped connection.
    expect(status()).toBe('Catching up with the output…');
    act(() => sockets.last().open());
    expect(status()).toBe('Live');
  });

  it('a stopped terminal offers Try again, which reconnects from the bytes received', async () => {
    const { sockets } = await live();
    act(() => sockets.last().output('abc'));
    act(() => sockets.last().drop(1011, 'input write timed out'));
    expect(status()).toBe('The terminal failed on its machine. (input write timed out)');
    expect(sockets.sockets).toHaveLength(1);
    fireEvent.click(screen.getByRole('button', { name: 'Try again' }));
    expect(sockets.sockets).toHaveLength(2);
    expect(sockets.last().from).toBe(3);
    expect(status()).toBe('Connecting…');
    expect(screen.queryByRole('button', { name: 'Try again' })).toBeNull();
    expect(document.activeElement).toBe(frame());
    act(() => sockets.last().open());
    expect(status()).toBe('Live');
  });

  it('tries again by itself when the machine comes back', async () => {
    let liveness: Machine['liveness'] = 'unverifiable';
    // The hub's machines, with this laptop's liveness as the test says.
    const machines: typeof fetch = async (input, init) => {
      const response = await fetch(input, init);
      if (new URL(String(input)).pathname !== '/v1/machines') return response;
      const list = ((await response.json()) as Machine[]).map((m) => (m.id === ID.laptop ? { ...m, liveness } : m));
      return new Response(JSON.stringify(list), { status: 200, headers: { 'Content-Type': 'application/json' } });
    };
    const { sockets, queryClient } = await renderTerminal({ fetch: machines });
    // The upgrade is refused; the hub says the machine is away.
    act(() => sockets.last().drop(1006));
    await eventually(() =>
      expect(status()).toBe('This laptop cannot be reached right now, so its terminal cannot be shown.'),
    );
    await new Promise((done) => setTimeout(done, 100));
    expect(sockets.sockets).toHaveLength(1);

    liveness = 'live';
    await act(() => queryClient.invalidateQueries({ queryKey: keys.machines }));
    await eventually(() => expect(sockets.sockets).toHaveLength(2));
    expect(status()).toBe('Connecting…');
  });

  it('when the program ends in control mode, focus goes from xterm to the frame', async () => {
    const { term, sockets } = await live();
    fireEvent.click(screen.getByRole('button', { name: 'Take control' }));
    expect(document.activeElement).toBe(term.textarea);
    act(() => sockets.last().text({ type: 'exit' }));
    expect(mode()).toBe('Viewing');
    expect(document.activeElement).toBe(frame());
  });

  it('clears a refused notice once keystrokes go through again', async () => {
    const { term } = await live();
    fireEvent.click(screen.getByRole('button', { name: 'Take control' }));
    act(() => term.type('x'.repeat(SEND_LIMIT + 1)));
    expect(screen.getByRole('alert').textContent).toMatch(/at most 1 MiB goes at once/);
    act(() => term.type('a'));
    expect(screen.queryByRole('alert')).toBeNull();
  });

  it('on truncation, cancels a cut escape sequence without counting it as output', async () => {
    const { term, sockets } = await live();
    act(() => sockets.last().output('\x1b]0;half a tit'));
    act(() => sockets.last().text({ type: 'truncated', from: 1_000 }));
    expect([...(term.written.at(-1) ?? [])]).toEqual([0x18]);
    act(() => sockets.last().drop(1006));
    await eventually(() => expect(sockets.sockets).toHaveLength(2));
    expect(sockets.last().from).toBe(1_000);
  });

  it('drops OSC 8 links whose target is longer than 2 KiB', async () => {
    const { term } = await renderTerminal();
    const link = term.osc.get(8);
    expect(link?.('id=a;https://example.test/' + 'x'.repeat(MAX_LINK_LENGTH))).toBe(true);
    expect(link?.(';' + 'h'.repeat(MAX_LINK_LENGTH + 1))).toBe(true);
    // Up to 2 KiB, and the closing `8;;`, are xterm's to handle.
    expect(link?.(';' + 'h'.repeat(MAX_LINK_LENGTH))).toBe(false);
    expect(link?.(';https://example.test/')).toBe(false);
    expect(link?.(';')).toBe(false);
  });

  it('does not hear from a controller Strict Mode replaced: no "Closed." flash', async () => {
    const { sockets } = await renderTerminal({ strict: true });
    expect(fakes.FakeTerminal.instances).toHaveLength(2);
    expect(fakes.FakeTerminal.instances[0]?.disposed).toBe(true);
    expect(sockets.sockets).toHaveLength(1);
    expect(status()).toBe('Connecting…');
  });

  it('releases everything on unmount: xterm, WebGL, the socket and the observers', async () => {
    const { term, sockets, unmount, environment } = await live();
    const webgl = fakes.FakeWebgl.instances[0];
    const theme = term.options.theme;
    unmount();
    expect(term.disposed).toBe(true);
    expect(webgl?.disposed).toBe(true);
    expect(sockets.last().closedWith?.code).toBe(1000);
    expect(environment.listeners).toBe(0);
    expect(term.element?.isConnected).toBe(false);
    // The theme observer is gone.
    document.documentElement.setAttribute('data-theme', 'dark');
    await new Promise((done) => setTimeout(done, 20));
    expect(term.options.theme).toBe(theme);
    // No reconnect after the fact.
    await new Promise((done) => setTimeout(done, 60));
    expect(sockets.sockets).toHaveLength(1);
  });
});
