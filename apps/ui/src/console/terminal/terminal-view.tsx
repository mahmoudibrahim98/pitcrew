// A session's terminal, live, in a lazy chunk: xterm.js loads only when a terminal is shown.
//
// View mode is the default: input is off, and the shell keeps all its keys. "Take control" (or
// Enter on the focused terminal) hands every key to the program, Ctrl K, J and B included, by
// marking the frame as owning the shell's keys; "Release" or Ctrl+Shift+X gives them back. The
// mode is always on screen and announced. See the console README ("The terminal").

import '@xterm/xterm/css/xterm.css';
import {
  useEffect,
  useEffectEvent,
  useId,
  useRef,
  useState,
  type FocusEvent,
  type KeyboardEvent,
} from 'react';
import { useApi, useGatewayWorkspace, useMachines, useOpenSocket, useSession } from '../../data/index.ts';
import { Button, Kbd } from '../../design/index.ts';
import { cx } from '../../lib/cx.ts';
import { isMac } from '../../lib/platform.ts';
import { ownsShellKeys } from '../../shell/index.ts';
import { useOpenExternal } from '../render/links.tsx';
import { TerminalController, type Renderer, type TerminalMode } from './controller.ts';
import { terminalDiagnosis } from './diagnose.ts';
import { RELEASE_KEYS, RELEASE_LABEL, RELEASE_SHORTCUT, viewModeKey } from './keys.ts';
import { useTerminalPrefs } from './prefs.ts';
import {
  INPUT_LIMIT,
  SEND_LIMIT,
  type SendResult,
  type TerminalSocketFactory,
  type TerminalSocketOptions,
  type TerminalStatus,
} from './socket.ts';

export interface TerminalViewProps {
  sessionId: string;
  /**
   * Opens the terminal socket for an API path. By default the data layer's `useOpenSocket()`: a
   * browser WebSocket in development, the desktop gateway in the app. Tests pass a fake.
   */
  socket?: TerminalSocketFactory | undefined;
  /** For tests: the socket's back-off, environment and timings. */
  socketOptions?: Pick<
    TerminalSocketOptions,
    'backoff' | 'connectTimeoutMs' | 'environment' | 'random' | 'resizeMs' | 'stableMs'
  >;
  className?: string | undefined;
}

const KIB = Math.round(INPUT_LIMIT / 1024);
const MIB = Math.round(SEND_LIMIT / 1024 / 1024);
const LINK_CLICK = isMac ? 'Cmd+click' : 'Ctrl+click';
/** Ctrl+V is the program's (^V), so pasting takes Shift as well, as in terminal emulators. */
const PASTE_KEYS = isMac ? 'Cmd+V' : 'Ctrl+Shift+V';

export function statusText(status: TerminalStatus): string {
  switch (status.kind) {
    case 'connecting':
      return 'Connecting…';
    case 'live':
      return 'Live';
    case 'reconnecting':
      return 'The connection dropped. Reconnecting…';
    case 'waiting':
      if (status.why === 'offline') return 'Offline. It reconnects when the network is back.';
      if (status.why === 'hidden') return 'Paused while this page is hidden.';
      return 'Catching up with the output…';
    case 'ended':
      return 'The program ended.';
    case 'stopped':
      return status.reason;
    case 'closed':
      return 'Closed.';
  }
}

const canTakeInput = (status: TerminalStatus) =>
  status.kind === 'live' || status.kind === 'connecting' || status.kind === 'reconnecting' || status.kind === 'waiting';

function inputMessage(result: SendResult): string | undefined {
  switch (result) {
    case 'sent':
      return undefined;
    case 'queued':
      return `Not connected: what you type is sent when the connection is back (up to ${KIB} KiB).`;
    case 'refused':
      return `Not sent: at most ${MIB} MiB goes at once, and at most ${KIB} KiB waits while disconnected.`;
    case 'closed':
      return 'Not sent: the terminal has closed.';
  }
}

export function TerminalView({ sessionId, socket, socketOptions, className }: TerminalViewProps) {
  const api = useApi();
  const openExternal = useOpenExternal();
  const screenReader = useTerminalPrefs((s) => s.screenReader);
  const setScreenReader = useTerminalPrefs((s) => s.setScreenReader);
  const frame = useRef<HTMLDivElement>(null);
  const host = useRef<HTMLDivElement>(null);
  const controller = useRef<TerminalController | null>(null);
  const modeRef = useRef<TerminalMode>('view');
  const [mode, setMode] = useState<TerminalMode>('view');
  const [status, setStatus] = useState<TerminalStatus>({ kind: 'connecting' });
  const [truncated, setTruncated] = useState(false);
  const [notice, setNotice] = useState<string | undefined>();
  const [renderer, setRenderer] = useState<Renderer | undefined>();
  const [linkHint, setLinkHint] = useState<string | undefined>();
  const hintId = useId();

  const openTransportSocket = useOpenSocket();
  const canControl = canTakeInput(status);
  const effective: TerminalMode = canControl ? mode : 'view';
  const control = effective === 'control';

  // Whether the session's machine can be reached, once both are known (and, in the desktop app,
  // whether the gateway can reach the workspace's daemon).
  const session = useSession(sessionId);
  const machines = useMachines();
  const workspace = useGatewayWorkspace();
  const machineId = session.data?.machine;
  const liveness = machines.data?.find((m) => m.id === machineId)?.liveness;
  const reachable =
    session.data === undefined || liveness === undefined
      ? undefined
      : liveness === 'live' && session.data.state !== 'unreachable' && (workspace?.state ?? 'ready') === 'ready';

  const takeControl = () => {
    const c = controller.current;
    if (c === null || !canControl) return;
    c.setMode('control');
    setMode('control');
    c.focus();
  };
  const release = () => {
    controller.current?.setMode('view');
    setMode('view');
    frame.current?.focus();
  };
  const tryAgain = () => {
    controller.current?.retry();
    // The button goes as it connects; focus stays on the terminal rather than falling to the page.
    frame.current?.focus();
  };

  // Callbacks for the controller: always the latest state, without recreating it.
  const openSocket = useEffectEvent((path: string) => (socket ?? openTransportSocket)(path));
  const openLink = useEffectEvent((url: string) => openExternal(url));
  const machineOf = useEffectEvent(() => machineId);
  const onRelease = useEffectEvent(() => release());
  const onStatus = useEffectEvent((next: TerminalStatus) => {
    setStatus(next);
    if (next.kind === 'live') setNotice(undefined);
    const c = controller.current;
    if ((next.kind === 'ended' || next.kind === 'stopped') && c?.mode === 'control') {
      // Control ends with the program; focus in the terminal goes to its frame, not the page.
      const inside = frame.current?.contains(document.activeElement) ?? false;
      c.setMode('view');
      setMode('view');
      if (inside) frame.current?.focus();
    }
  });
  const onInput = useEffectEvent((result: SendResult) => setNotice(inputMessage(result)));
  const options = useEffectEvent(() => socketOptions);

  useEffect(() => {
    modeRef.current = effective;
    controller.current?.setMode(effective);
  }, [effective]);

  useEffect(() => {
    controller.current?.setScreenReader(screenReader);
  }, [screenReader]);

  // The machine (or the workspace) came back: a terminal that stopped while it was away tries
  // again.
  const wasReachable = useRef<boolean | undefined>(undefined);
  useEffect(() => {
    const was = wasReachable.current;
    wasReachable.current = reachable;
    if (reachable === true && was === false) controller.current?.retry();
  }, [reachable]);

  useEffect(() => {
    const element = host.current;
    if (element === null) return;
    const extra = options();
    // A controller that has been replaced (Strict Mode mounts twice) is not heard from again.
    let current = true;
    const c = new TerminalController({
      host: element,
      sessionId,
      socket: (path) => openSocket(path),
      diagnose: terminalDiagnosis(api, sessionId, { machineOf: () => machineOf() }),
      screenReader: useTerminalPrefs.getState().screenReader,
      openLink: (url) => openLink(url),
      onLinkHint: (url) => {
        if (current) setLinkHint(url);
      },
      onStatus: (next) => {
        if (current) onStatus(next);
      },
      onTruncated: () => {
        if (current) setTruncated(true);
      },
      onRenderer: (next) => {
        if (current) setRenderer(next);
      },
      onRelease: () => {
        if (current) onRelease();
      },
      onInput: (result) => {
        if (current) onInput(result);
      },
      ...(extra === undefined ? {} : { socketOptions: extra }),
    });
    c.setMode(modeRef.current);
    controller.current = c;
    return () => {
      current = false;
      controller.current = null;
      c.dispose();
    };
  }, [sessionId, api]);

  // View mode keeps focus on the frame, so the shell's keys work; control mode keeps it in xterm.
  const onFocus = (event: FocusEvent<HTMLDivElement>) => {
    const c = controller.current;
    if (c === null) return;
    if (c.mode === 'view' && c.ownsInput(event.target)) frame.current?.focus();
    else if (c.mode === 'control' && event.target === frame.current) c.focus();
  };

  const onKeyDown = (event: KeyboardEvent<HTMLDivElement>) => {
    const c = controller.current;
    if (c === null || c.mode !== 'view') return;
    const key = viewModeKey(event);
    if (key === undefined) return;
    switch (key.kind) {
      case 'control':
        if (!canControl) return;
        // Also keeps the Enter from reaching the program once it has control.
        event.preventDefault();
        takeControl();
        return;
      case 'copy': {
        // The person's own selection; the program can never write the clipboard.
        const text = c.selection();
        if (text === '') return;
        event.preventDefault();
        void navigator.clipboard?.writeText(text).catch(() => {});
        return;
      }
      case 'scroll':
        event.preventDefault();
        c.scrollLines(key.lines);
        return;
      case 'page':
        event.preventDefault();
        c.scrollPages(key.pages);
        return;
      case 'top':
        event.preventDefault();
        c.scrollToTop();
        return;
      case 'bottom':
        event.preventDefault();
        c.scrollToBottom();
        return;
    }
  };

  const announcement = control
    ? `You have control of the terminal: keys go to the program. ${RELEASE_LABEL} or Release gives them back.`
    : canControl
      ? 'Viewing the terminal: input is off. Enter or Take control lets you type.'
      : 'Viewing the terminal: input is off.';

  return (
    <div className={cx('flex h-full min-h-0 flex-col', className)}>
      <div className="flex min-h-10 shrink-0 flex-wrap items-center gap-x-3 gap-y-1 border-b border-line px-3 py-1.5">
        <span
          data-testid="terminal-mode"
          className={cx(
            'inline-flex items-center gap-1.5 rounded-pill px-2 py-0.5 text-xs font-medium',
            control ? 'bg-warn-soft text-ink' : 'bg-sunken text-ink-2',
          )}
        >
          <span aria-hidden className={cx('size-1.5 rounded-full', control ? 'bg-warn' : 'bg-muted')} />
          {control ? 'In control' : 'Viewing'}
        </span>
        {control ? (
          <Button onClick={release} aria-keyshortcuts={RELEASE_SHORTCUT}>
            Release
          </Button>
        ) : (
          <Button onClick={takeControl} disabled={!canControl}>
            Take control
          </Button>
        )}
        {status.kind === 'stopped' && <Button onClick={tryAgain}>Try again</Button>}
        <span className="inline-flex items-center gap-1 text-xs text-ink-2">
          {control ? (
            <>
              <Kbd keys={RELEASE_KEYS} /> releases
            </>
          ) : (
            canControl && (
              <>
                <Kbd keys={['Enter']} /> takes control
              </>
            )
          )}
        </span>
        {/* Wraps rather than truncates: a stop's reason is the one thing to read here. */}
        <span role="status" data-testid="terminal-status" className="min-w-0 text-xs break-words text-ink-2">
          {statusText(status)}
        </span>
        <label className="ml-auto inline-flex items-center gap-1.5 text-xs text-ink-2">
          <input
            type="checkbox"
            checked={screenReader}
            onChange={(event) => setScreenReader(event.target.checked)}
            className="accent-accent"
          />
          Screen reader mode
        </label>
      </div>
      {notice !== undefined && (
        <p role="alert" className="shrink-0 border-b border-line bg-warn-soft px-3 py-1 text-xs text-ink">
          {notice}
        </p>
      )}
      <p aria-live="polite" className="sr-only">
        {announcement}
      </p>
      <div
        ref={frame}
        role="group"
        aria-label="Terminal"
        aria-describedby={hintId}
        tabIndex={0}
        data-terminal-focus=""
        data-terminal-mode={effective}
        data-renderer={renderer}
        title={linkHint === undefined ? undefined : `${LINK_CLICK} to open ${linkHint}`}
        onFocus={onFocus}
        onKeyDown={onKeyDown}
        {...(control ? ownsShellKeys : {})}
        className={cx(
          'flex min-h-0 flex-1 flex-col bg-bg outline-none',
          'focus-within:outline-2 focus-within:-outline-offset-2 focus-within:outline-solid',
          control ? 'focus-within:outline-warn' : 'focus-within:outline-accent',
        )}
      >
        {truncated && (
          <p data-testid="terminal-truncated" className="shrink-0 border-b border-dashed border-line-2 px-3 py-0.5 font-mono text-xs text-ink-2">
            Earlier output is no longer available.
          </p>
        )}
        <div className="relative min-h-0 flex-1">
          <div ref={host} className="absolute inset-0 py-1 pl-2" />
        </div>
      </div>
      <p id={hintId} className="sr-only">
        {control
          ? `Keys go to the program, Escape and Tab included. Paste with ${PASTE_KEYS} or the context menu. ${RELEASE_LABEL} releases control.`
          : 'Input is off. Press Enter to take control. The arrow keys, Page Up, Page Down, Home and End scroll.'}
      </p>
    </div>
  );
}
