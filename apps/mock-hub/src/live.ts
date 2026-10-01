// The two WebSockets: the event stream (`GET /v1/stream`) and session terminals
// (`GET /v1/sessions/{id}/terminal`). The server checks the handshake and the token; these
// functions run the accepted connections.

import { requireReachable } from './routes.ts';
import type { Hub } from './state.ts';
import type { Engine, Event, Session, StreamFrame } from './types.ts';
import { isRecord, notFound, queryInt } from './validate.ts';
import type { WebSocketConnection } from './ws.ts';

const PING_MS = 20_000;
/** The most events one `events` frame carries when catching a client up. */
const REPLAY_BATCH = 500;
/** Terminals of sessions that ran longer than this pretend their replay buffer lost the start. */
const DAY_MS = 24 * 60 * 60 * 1000;
/** Where that pretend replay buffer starts, as an offset in the terminal's output. */
const TRUNCATED_FROM = 4 * 1024 * 1024;

const ENGINE_NAMES: Record<Engine, string> = {
  claude: 'Claude Code',
  codex: 'Codex',
  opencode: 'OpenCode',
};

// ─── Event stream ───────────────────────────────────────────────────────────────────────────────

/** The `since` revision of a stream request, if any. */
export function streamSince(query: URLSearchParams): number | undefined {
  return queryInt(query, 'since', 0);
}

/**
 * Sends `hello`, then the events after `since` (if the client is behind), then every new batch.
 * Each connection keeps its own cursor, so a client never gets an event twice.
 */
export function openStream(hub: Hub, conn: WebSocketConnection, since: number | undefined): void {
  let cursor = hub.rev;
  sendFrame(conn, { type: 'hello', rev: cursor, log: hub.logId });
  if (since !== undefined && since < cursor) {
    sendEvents(conn, since, hub.eventsAfter(since));
  }
  const unsubscribe = hub.subscribe(() => {
    const fresh = hub.eventsAfter(cursor);
    if (fresh.length > 0) {
      sendEvents(conn, cursor, fresh);
      cursor += fresh.length;
    }
  });
  const ping = setInterval(() => sendFrame(conn, { type: 'ping', at: Date.now() }), PING_MS);
  void conn.closed.then(() => {
    unsubscribe();
    clearInterval(ping);
  });
}

/** Sends `events` (which follow revision `after`) as one or more `events` frames. */
function sendEvents(conn: WebSocketConnection, after: number, events: Event[]): void {
  for (let i = 0; i < events.length; i += REPLAY_BATCH) {
    const batch = events.slice(i, i + REPLAY_BATCH);
    sendFrame(conn, {
      type: 'events',
      from_rev: after + i + 1,
      to_rev: after + i + batch.length,
      events: batch,
    });
  }
}

function sendFrame(conn: WebSocketConnection, frame: StreamFrame): void {
  conn.sendText(JSON.stringify(frame));
}

// ─── Terminals ──────────────────────────────────────────────────────────────────────────────────

export interface TerminalTarget {
  session: Session;
  cols: number;
  rows: number;
}

/** The session behind a terminal request: 404 if unknown or without a terminal, 503 if unreachable. */
export function terminalTarget(hub: Hub, ref: string, query: URLSearchParams): TerminalTarget {
  const session = hub.findSession(ref);
  if (session === undefined) {
    throw notFound(`No session ${ref}.`);
  }
  requireReachable(hub, session);
  if (session.terminal === undefined) {
    throw notFound(`Session ${session.id} has no terminal.`);
  }
  return {
    session,
    cols: Math.min(queryInt(query, 'cols', 1) ?? 80, 1000),
    rows: Math.min(queryInt(query, 'rows', 1) ?? 24, 1000),
  };
}

/**
 * Replays a short canned screen, then echoes every keystroke back. Sessions that ran longer
 * than a day pretend their replay buffer lost the start (`truncated`). When the session ends,
 * the client gets `{"type":"exit"}` and the socket closes.
 */
export function openTerminal(hub: Hub, conn: WebSocketConnection, target: TerminalTarget): void {
  const { session } = target;
  if (session.last_activity - session.started > DAY_MS) {
    conn.sendText(JSON.stringify({ type: 'truncated', from: TRUNCATED_FROM }));
  }
  for (const chunk of cannedScreen(session, target.cols)) {
    conn.sendBinary(Buffer.from(chunk, 'utf8'));
  }
  if (session.state === 'ended') {
    exit(conn);
    return;
  }
  conn.onBinary = (keys) => conn.sendBinary(keys);
  conn.onText = (text) => {
    // There is no real terminal to resize, so a valid resize has no effect.
    if (!isValidControl(text)) {
      conn.close(1007, 'expected {"type":"resize","cols":N,"rows":N}');
    }
  };
  const unsubscribe = hub.subscribe(() => {
    if (session.state === 'ended') {
      exit(conn);
    }
  });
  void conn.closed.then(unsubscribe);
}

function exit(conn: WebSocketConnection): void {
  conn.sendText(JSON.stringify({ type: 'exit' }));
  conn.close(1000, 'session ended');
}

/**
 * Whether a control message is well formed: a resize with sensible sizes, or a message type this
 * mock does not know (accepted and ignored, so newer clients keep working).
 */
function isValidControl(text: string): boolean {
  let message: unknown;
  try {
    message = JSON.parse(text);
  } catch {
    return false;
  }
  if (!isRecord(message) || typeof message['type'] !== 'string') {
    return false;
  }
  const size = (n: unknown): boolean =>
    typeof n === 'number' && Number.isInteger(n) && n >= 1 && n <= 1000;
  return message['type'] !== 'resize' || (size(message['cols']) && size(message['rows']));
}

/** A short ANSI screen in two chunks: a header, then the status and a prompt. */
function cannedScreen(session: Session, cols: number): string[] {
  const rule = '─'.repeat(Math.max(10, Math.min(cols, 120)));
  const where = session.branch === undefined ? session.cwd : `${session.cwd} (${session.branch})`;
  const status = session.status_line ?? session.state;
  return [
    `\x1b[2J\x1b[H\x1b[1;36m${ENGINE_NAMES[session.engine]}\x1b[0m  ${session.title ?? session.id}\r\n` +
      `\x1b[2m${where}\x1b[0m\r\n${rule}\r\n`,
    `\x1b[33m●\x1b[0m ${status}\r\n\r\n` +
      `\x1b[2mMock terminal: what you type is echoed back.\x1b[0m\r\n${rule}\r\n> `,
  ];
}
