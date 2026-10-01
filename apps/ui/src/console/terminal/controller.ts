// One terminal: xterm.js on a `TerminalSocket`, with no React. The view creates it on mount and
// disposes it on unmount, which releases everything: the socket, the observers, the WebGL context
// and xterm itself.
//
// - Rendering: WebGL, or xterm's DOM renderer when WebGL is unavailable or its context is lost.
// - Size: fitted to the host element, and kept fitted as it resizes.
// - Theme and font: the design tokens, read again when the theme changes.
// - Input: only in control mode, and the release chord never reaches the program.
// - Flow: output not yet parsed is bounded; past 4 MiB the socket holds (closes) until xterm has
//   caught up, then resumes from the bytes received, so a flood costs neither memory nor output.

import { FitAddon } from '@xterm/addon-fit';
import { WebglAddon } from '@xterm/addon-webgl';
import { Terminal, type IDisposable } from '@xterm/xterm';
import { isReleaseChord } from './keys.ts';
import { linkHandler, SWALLOWED_OSC, terminalOptions } from './options.ts';
import {
  TerminalSocket,
  type SendResult,
  type TerminalProblem,
  type TerminalSocketFactory,
  type TerminalSocketOptions,
  type TerminalStatus,
} from './socket.ts';
import { rootTokens, terminalFont, terminalTheme, type TerminalFont, type TokenReader } from './theme.ts';

export type TerminalMode = 'view' | 'control';
export type Renderer = 'webgl' | 'dom';

/** Output given to xterm and not yet parsed: the socket holds above HIGH until it drains to LOW. */
export const HIGH_WATER = 4 * 1024 * 1024;
export const LOW_WATER = 512 * 1024;
/** How long to wait for the terminal font before measuring cells with whatever is there. */
const FONT_WAIT_MS = 1_500;

export interface ControllerOptions {
  /** The element xterm fills. */
  host: HTMLElement;
  sessionId: string;
  socket: TerminalSocketFactory;
  diagnose?: (() => Promise<TerminalProblem | undefined>) | undefined;
  screenReader: boolean;
  openLink(url: string): void;
  onLinkHint(url: string | undefined): void;
  onStatus(status: TerminalStatus): void;
  onTruncated(): void;
  onRenderer(renderer: Renderer): void;
  /** The release chord, pressed in control mode. */
  onRelease(): void;
  /** Keystrokes that were not sent at once: queued, refused, or after the end. */
  onInput(result: Exclude<SendResult, 'sent'>): void;
  tokens?: TokenReader;
  /** For tests: the socket's back-off, environment and timings. */
  socketOptions?: Pick<TerminalSocketOptions, 'backoff' | 'environment' | 'random' | 'resizeMs' | 'stableMs'>;
}

function waitForFont(font: TerminalFont): Promise<void> {
  const fonts = typeof document === 'undefined' ? undefined : document.fonts;
  if (fonts === undefined || typeof fonts.load !== 'function') return Promise.resolve();
  const loaded = fonts.load(`${font.fontSize}px ${font.fontFamily}`).then(
    () => undefined,
    () => undefined,
  );
  return Promise.race([loaded, new Promise<void>((done) => setTimeout(done, FONT_WAIT_MS))]);
}

export class TerminalController {
  readonly #options: ControllerOptions;
  readonly #read: TokenReader;
  readonly #term: Terminal;
  readonly #fit = new FitAddon();
  readonly #socket: TerminalSocket;
  readonly #disposables: IDisposable[] = [];
  readonly #cleanups: (() => void)[] = [];
  #webgl: WebglAddon | undefined;
  #mode: TerminalMode = 'view';
  /** Bytes written to xterm and not yet parsed. */
  #pending = 0;
  #holding = false;
  #disposed = false;
  #frame = 0;

  constructor(options: ControllerOptions) {
    this.#options = options;
    this.#read = options.tokens ?? rootTokens;
    const font = terminalFont(this.#read);
    const links = linkHandler(
      (url) => options.openLink(url),
      (url) => options.onLinkHint(url),
    );
    this.#term = new Terminal(
      terminalOptions({ theme: terminalTheme(this.#read), font, screenReader: options.screenReader, links }),
    );
    for (const code of SWALLOWED_OSC) {
      this.#disposables.push(this.#term.parser.registerOscHandler(code, () => true));
    }
    this.#term.attachCustomKeyEventHandler((event) => this.#key(event));
    this.#socket = new TerminalSocket({
      ...options.socketOptions,
      sessionId: options.sessionId,
      socket: options.socket,
      size: { cols: this.#term.cols, rows: this.#term.rows },
      onOutput: (bytes) => this.#write(bytes),
      onTruncated: () => options.onTruncated(),
      onStatus: (status) => options.onStatus(status),
      ...(options.diagnose === undefined ? {} : { diagnose: options.diagnose }),
    });
    this.#disposables.push(
      this.#term.onData((data) => this.#input(data)),
      // Mouse reports in X10 mode: one byte per character.
      this.#term.onBinary((data) => this.#input(Uint8Array.from(data, (c) => c.charCodeAt(0) & 0xff))),
      this.#term.onResize(({ cols, rows }) => this.#socket.resize(cols, rows)),
    );
    this.#term.loadAddon(this.#fit);
    void waitForFont(font).then(() => this.#open());
  }

  get mode(): TerminalMode {
    return this.#mode;
  }

  /** View mode: input off. Control mode: keys go to the program. */
  setMode(mode: TerminalMode): void {
    this.#mode = mode;
    if (!this.#disposed) this.#term.options.disableStdin = mode !== 'control';
  }

  setScreenReader(on: boolean): void {
    if (!this.#disposed) this.#term.options.screenReaderMode = on;
  }

  /** Focuses xterm's input, for control mode. */
  focus(): void {
    if (!this.#disposed) this.#term.focus();
  }

  /** Whether `element` is xterm's own input (which only control mode should hold). */
  ownsInput(element: EventTarget | null): boolean {
    return element !== null && element === this.#term.textarea;
  }

  scrollLines(lines: number): void {
    this.#term.scrollLines(lines);
  }

  scrollPages(pages: number): void {
    this.#term.scrollPages(pages);
  }

  scrollToTop(): void {
    this.#term.scrollToTop();
  }

  scrollToBottom(): void {
    this.#term.scrollToBottom();
  }

  /** The text the person selected, if any. */
  selection(): string {
    return this.#term.hasSelection() ? this.#term.getSelection() : '';
  }

  dispose(): void {
    if (this.#disposed) return;
    this.#disposed = true;
    if (this.#frame !== 0) cancelAnimationFrame(this.#frame);
    this.#socket.stop();
    for (const cleanup of this.#cleanups.splice(0)) cleanup();
    for (const disposable of this.#disposables.splice(0)) disposable.dispose();
    // Releases the WebGL context before xterm goes (xterm would dispose it too).
    this.#webgl?.dispose();
    this.#webgl = undefined;
    this.#term.dispose();
  }

  #open(): void {
    if (this.#disposed) return;
    const { host } = this.#options;
    this.#term.open(host);
    // The view's frame is the tab stop; xterm's input is focused only in control mode.
    if (this.#term.textarea !== undefined) this.#term.textarea.tabIndex = -1;
    this.#loadWebgl();
    this.#watch(host);
    this.#fitNow();
    this.#socket.start();
  }

  #loadWebgl(): void {
    let addon: WebglAddon | undefined;
    try {
      addon = new WebglAddon();
      this.#term.loadAddon(addon);
    } catch {
      // No WebGL 2 here: xterm keeps its DOM renderer.
      try {
        addon?.dispose();
      } catch {
        // It never activated.
      }
      this.#options.onRenderer('dom');
      return;
    }
    const loaded = addon;
    this.#webgl = loaded;
    this.#disposables.push(
      loaded.onContextLoss(() => {
        // Disposing the addon puts xterm's DOM renderer back.
        if (this.#webgl === loaded) this.#webgl = undefined;
        loaded.dispose();
        this.#options.onRenderer('dom');
      }),
    );
    this.#options.onRenderer('webgl');
  }

  #watch(host: HTMLElement): void {
    if (typeof ResizeObserver === 'function') {
      const observer = new ResizeObserver(() => this.#scheduleFit());
      observer.observe(host);
      this.#cleanups.push(() => observer.disconnect());
    }
    const retheme = () => {
      if (!this.#disposed) this.#term.options.theme = terminalTheme(this.#read);
    };
    // The theme is `data-theme` on the root, or the system's when it has none.
    const mutations = new MutationObserver(retheme);
    mutations.observe(document.documentElement, { attributes: true, attributeFilter: ['data-theme'] });
    this.#cleanups.push(() => mutations.disconnect());
    const scheme = typeof window.matchMedia === 'function' ? window.matchMedia('(prefers-color-scheme: dark)') : undefined;
    scheme?.addEventListener('change', retheme);
    this.#cleanups.push(() => scheme?.removeEventListener('change', retheme));
  }

  #scheduleFit(): void {
    if (this.#frame !== 0 || this.#disposed) return;
    this.#frame = requestAnimationFrame(() => {
      this.#frame = 0;
      this.#fitNow();
    });
  }

  #fitNow(): void {
    if (this.#disposed) return;
    try {
      // Does nothing while the host has no size (hidden); the next resize fits it.
      this.#fit.fit();
    } catch {
      // Measuring needs a laid-out element.
    }
  }

  #write(bytes: Uint8Array): void {
    if (this.#disposed) return;
    const size = bytes.byteLength;
    this.#pending += size;
    this.#term.write(bytes, () => {
      this.#pending -= size;
      if (this.#holding && this.#pending <= LOW_WATER && !this.#disposed) {
        this.#holding = false;
        this.#socket.resume();
      }
    });
    if (!this.#holding && this.#pending > HIGH_WATER) {
      this.#holding = true;
      this.#socket.hold();
    }
  }

  #input(data: string | Uint8Array): void {
    if (this.#mode !== 'control' || this.#disposed) return;
    const result = this.#socket.send(data);
    if (result !== 'sent') this.#options.onInput(result);
  }

  /** xterm asks before acting on a key: false leaves the key alone. */
  #key(event: KeyboardEvent): boolean {
    if (this.#mode !== 'control') return false;
    if (isReleaseChord(event)) {
      if (event.type === 'keydown') {
        event.preventDefault();
        this.#options.onRelease();
      }
      return false;
    }
    return true;
  }
}
