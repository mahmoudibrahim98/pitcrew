// TerminalSocket against a fake socket and a fake hub: output counted across reconnects with no
// byte lost or repeated, truncation, the end, every close code, the back-off, pausing, the
// keystroke cap, resizes, and the token kept out of the URL.

import { afterEach, beforeEach, describe, expect, it, vi } from 'vitest';
import { ApiError, type Machine, type Session } from '../../data/index.ts';
import { terminalDiagnosis } from '../terminal/diagnose.ts';
import {
  browserSocketFactory,
  INPUT_LIMIT,
  TerminalSocket,
  terminalPath,
  type TerminalProblem,
  type TerminalSocketOptions,
  type TerminalStatus,
} from '../terminal/socket.ts';
import { FakeEnvironment, FakeSocket } from './terminal-fakes.ts';

function setup(options: Partial<TerminalSocketOptions> = {}) {
  const sockets: FakeSocket[] = [];
  const output: number[] = [];
  const statuses: TerminalStatus[] = [];
  const truncations: { from: number; previous: number }[] = [];
  const environment = new FakeEnvironment();
  const terminal = new TerminalSocket({
    sessionId: '01JB000000000000000SES0001',
    socket: (path) => {
      const socket = new FakeSocket(path);
      sockets.push(socket);
      return socket;
    },
    size: { cols: 80, rows: 24 },
    onOutput: (bytes) => output.push(...bytes),
    onStatus: (status) => statuses.push(status),
    onTruncated: (jump) => truncations.push(jump),
    environment,
    random: () => 0.5,
    backoff: { initialMs: 100, maxMs: 1_600 },
    ...options,
  });
  const last = () => {
    const socket = sockets.at(-1);
    if (socket === undefined) throw new Error('no socket opened');
    return socket;
  };
  return { terminal, sockets, output, statuses, truncations, environment, last };
}

const status = (statuses: TerminalStatus[]) => statuses.at(-1);

/** Lets settled promises run their callbacks (a diagnosis answering). */
async function flush(): Promise<void> {
  for (let i = 0; i < 5; i += 1) await Promise.resolve();
}

beforeEach(() => {
  vi.useFakeTimers();
});

afterEach(() => {
  vi.useRealTimers();
});

describe('the terminal path and the token', () => {
  it('puts the token only in the subprotocols, never in the URL', () => {
    const opened: { url: string; protocols: string[] }[] = [];
    const factory = browserSocketFactory({
      baseUrl: 'http://127.0.0.1:47317',
      token: 'secret-device-token',
      create: (url, protocols) => {
        opened.push({ url, protocols });
        return new FakeSocket(url);
      },
    });
    factory(terminalPath('01JB000000000000000SES0001', { cols: 120, rows: 40 }, 4096));
    expect(opened).toEqual([
      {
        url: 'ws://127.0.0.1:47317/v1/sessions/01JB000000000000000SES0001/terminal?cols=120&rows=40&from=4096',
        protocols: ['pitcrew.v1', 'pitcrew.bearer.secret-device-token'],
      },
    ]);
    expect(opened[0]?.url).not.toContain('secret');
  });

  it('keeps a path prefix, uses wss for https, and offers no bearer without a token', () => {
    const opened: { url: string; protocols: string[] }[] = [];
    const factory = browserSocketFactory({
      baseUrl: 'https://hub.example.test/pitcrew/',
      create: (url, protocols) => {
        opened.push({ url, protocols });
        return new FakeSocket(url);
      },
    });
    factory(terminalPath('a/b', { cols: 80, rows: 24 }, 0));
    expect(opened).toEqual([
      { url: 'wss://hub.example.test/pitcrew/v1/sessions/a%2Fb/terminal?cols=80&rows=24&from=0', protocols: ['pitcrew.v1'] },
    ]);
  });

  it('asks the factory for a path with the size and the offset, never a URL or a token', () => {
    const { terminal, last } = setup();
    terminal.start();
    expect(last().path).toBe('/v1/sessions/01JB000000000000000SES0001/terminal?cols=80&rows=24&from=0');
  });
});

describe('output', () => {
  it('is counted across reconnects, with no byte lost or repeated', async () => {
    // A hub whose terminal has produced `produced`; each connection replays from its `from`.
    const produced = Array.from({ length: 1_000 }, (_, i) => i % 251);
    const { terminal, sockets, output, last } = setup();
    const serve = (socket: FakeSocket, upTo: number, chunk: number) => {
      for (let at = socket.from; at < upTo; at += chunk) {
        socket.output(Uint8Array.from(produced.slice(at, Math.min(upTo, at + chunk))));
      }
    };
    terminal.start();
    last().open();
    serve(last(), 400, 64);
    last().drop(1006);
    await vi.advanceTimersByTimeAsync(1_000);
    expect(last().from).toBe(400);
    last().open();
    serve(last(), 777, 50);
    last().drop(1013);
    await vi.advanceTimersByTimeAsync(1_000);
    expect(last().from).toBe(777);
    last().open();
    serve(last(), 1_000, 99);
    expect(sockets).toHaveLength(3);
    expect(output).toEqual(produced);
    expect(terminal.received).toBe(1_000);
  });

  it('jumps to `truncated.from` at the start, and reconnects from there', async () => {
    const { terminal, output, truncations, last } = setup();
    terminal.start();
    last().open();
    last().text({ type: 'truncated', from: 4 * 1024 * 1024 });
    last().output('screen');
    expect(truncations).toEqual([{ from: 4 * 1024 * 1024, previous: 0 }]);
    expect(terminal.received).toBe(4 * 1024 * 1024 + 6);
    expect(String.fromCharCode(...output)).toBe('screen');
    last().drop(1006);
    await vi.advanceTimersByTimeAsync(1_000);
    expect(last().from).toBe(4 * 1024 * 1024 + 6);
  });

  it('jumps to `truncated.from` in the middle', async () => {
    const { terminal, truncations, last } = setup();
    terminal.start();
    last().open();
    last().output('0123456789');
    last().text({ type: 'truncated', from: 5_000 });
    last().output('abc');
    expect(truncations).toEqual([{ from: 5_000, previous: 10 }]);
    expect(terminal.received).toBe(5_003);
    last().drop(1001);
    await vi.advanceTimersByTimeAsync(1_000);
    expect(last().from).toBe(5_003);
  });

  it('ignores malformed and unknown control frames', () => {
    const { terminal, truncations, statuses, last } = setup();
    terminal.start();
    last().open();
    last().onmessage?.({ data: '{not json' });
    last().text({ type: 'truncated', from: -1 });
    last().text({ type: 'truncated', from: 1.5 });
    last().text({ type: 'truncated' });
    last().text({ type: 'something_new', from: 99 });
    last().text(['exit']);
    expect(truncations).toEqual([]);
    expect(terminal.received).toBe(0);
    expect(status(statuses)).toEqual({ kind: 'live' });
  });
});

describe('the end', () => {
  it('stops at `exit` and never reconnects', async () => {
    const { terminal, sockets, statuses, last } = setup();
    terminal.start();
    last().open();
    last().output('bye\r\n');
    last().text({ type: 'exit' });
    expect(status(statuses)).toEqual({ kind: 'ended' });
    expect(last().closedWith?.code).toBe(1000);
    // The hub's own close after `exit` arrives on a socket already let go.
    last().drop(1000);
    await vi.advanceTimersByTimeAsync(60_000);
    expect(sockets).toHaveLength(1);
    expect(terminal.send('x')).toBe('closed');
  });

  it('stops at a 1000 close and never reconnects', async () => {
    const { terminal, sockets, statuses, last } = setup();
    terminal.start();
    last().open();
    last().drop(1000, 'session ended');
    expect(status(statuses)).toEqual({ kind: 'ended' });
    await vi.advanceTimersByTimeAsync(60_000);
    expect(sockets).toHaveLength(1);
  });
});

describe('close codes', () => {
  it.each([
    [1007, 'malformed control message'],
    [1009, 'too large'],
    [1011, 'failed on its machine'],
  ])('%i stops, says why, and never loops', async (code, words) => {
    const { terminal, sockets, statuses, last } = setup();
    terminal.start();
    last().open();
    last().drop(code, 'detail from the hub');
    const final = status(statuses);
    expect(final?.kind).toBe('stopped');
    expect(final?.kind === 'stopped' && final.reason).toContain(words);
    expect(final?.kind === 'stopped' && final.reason).toContain('detail from the hub');
    expect(final?.kind === 'stopped' && final.code).toBe(code);
    await vi.advanceTimersByTimeAsync(60_000);
    expect(sockets).toHaveLength(1);
  });

  it.each([1001, 1013, 1006, 1005, 1012])('%i reconnects from the bytes received', async (code) => {
    const { terminal, sockets, statuses, last } = setup();
    terminal.start();
    last().open();
    last().output('abc');
    last().drop(code);
    expect(status(statuses)?.kind).toBe('reconnecting');
    await vi.advanceTimersByTimeAsync(1_000);
    expect(sockets).toHaveLength(2);
    expect(last().from).toBe(3);
  });

  it.each([
    [503, 'gpu-box cannot be reached right now, so its terminal cannot be shown.'],
    [404, 'This session has no terminal.'],
  ])('a refused upgrade the diagnosis explains as %i stops with the reason', async (code, message) => {
    const problem: TerminalProblem = { status: code, message };
    const diagnose = vi.fn(async () => problem);
    const { terminal, sockets, statuses, last } = setup({ diagnose });
    terminal.start();
    // A browser reports any refused upgrade as a 1006 close before `open`.
    last().drop(1006);
    await flush();
    expect(diagnose).toHaveBeenCalledTimes(1);
    expect(status(statuses)).toEqual({ kind: 'stopped', reason: message, status: code });
    await vi.advanceTimersByTimeAsync(60_000);
    expect(sockets).toHaveLength(1);
  });

  it('a refused upgrade with nothing to explain it (the network) reconnects', async () => {
    const diagnose = vi.fn(async () => undefined);
    const { terminal, sockets, last } = setup({ diagnose });
    terminal.start();
    last().drop(1006);
    await vi.advanceTimersByTimeAsync(1_000);
    expect(diagnose).toHaveBeenCalledTimes(1);
    expect(sockets).toHaveLength(2);
  });

  it('a diagnosis that fails counts as the network', async () => {
    const diagnose = vi.fn(async (): Promise<TerminalProblem | undefined> => {
      throw new Error('offline');
    });
    const { terminal, sockets, last } = setup({ diagnose });
    terminal.start();
    last().drop(1006);
    await vi.advanceTimersByTimeAsync(1_000);
    expect(sockets).toHaveLength(2);
  });

  it('a transport that knows the HTTP status (503, 404) stops without asking', async () => {
    for (const code of [503, 404]) {
      const diagnose = vi.fn(async () => undefined);
      const { terminal, sockets, statuses, last } = setup({ diagnose });
      terminal.start();
      last().drop(1006, '', code);
      expect(status(statuses)).toMatchObject({ kind: 'stopped', status: code });
      await vi.advanceTimersByTimeAsync(60_000);
      expect(sockets).toHaveLength(1);
      expect(diagnose).not.toHaveBeenCalled();
    }
  });

  it('a diagnosis that lands after stop() is ignored', async () => {
    let answer: (problem: TerminalProblem | undefined) => void = () => {};
    const diagnose = () => new Promise<TerminalProblem | undefined>((resolve) => (answer = resolve));
    const { terminal, statuses, last } = setup({ diagnose });
    terminal.start();
    last().drop(1006);
    terminal.stop();
    answer({ status: 503, message: 'late' });
    await flush();
    expect(status(statuses)).toEqual({ kind: 'closed' });
  });
});

describe('back-off', () => {
  /** The delays between failed attempts, read off the status. */
  async function delays(random: number, failures: number) {
    const { terminal, statuses, last, sockets } = setup({ random: () => random });
    terminal.start();
    const seen: number[] = [];
    for (let i = 0; i < failures; i += 1) {
      last().drop(1006);
      const now = status(statuses);
      if (now?.kind !== 'reconnecting') throw new Error(`expected reconnecting, got ${now?.kind}`);
      seen.push(now.delayMs);
      // Not a moment early.
      const count = sockets.length;
      await vi.advanceTimersByTimeAsync(Math.max(0, now.delayMs - 1));
      expect(sockets).toHaveLength(count);
      await vi.advanceTimersByTimeAsync(1);
      expect(sockets).toHaveLength(count + 1);
    }
    return seen;
  }

  it('doubles from the initial delay up to the cap, with jitter between half and all of it', async () => {
    const ceilings = [100, 200, 400, 800, 1_600, 1_600, 1_600];
    expect(await delays(0, 7)).toEqual(ceilings.map((c) => c * 0.5));
    const high = await delays(0.999, 7);
    high.forEach((delay, i) => {
      const ceiling = ceilings[i] ?? 0;
      expect(delay).toBeGreaterThan(ceiling * 0.99);
      expect(delay).toBeLessThanOrEqual(ceiling);
    });
  });

  it('starts over once a connection has stayed up', async () => {
    const { terminal, statuses, last } = setup({ stableMs: 5_000 });
    terminal.start();
    for (let i = 0; i < 4; i += 1) {
      last().drop(1006);
      await vi.advanceTimersByTimeAsync(2_000);
    }
    last().open();
    await vi.advanceTimersByTimeAsync(5_000);
    last().drop(1006);
    expect(status(statuses)).toEqual({ kind: 'reconnecting', attempt: 1, delayMs: 75 });
  });

  it('does not start over for a connection that drops at once', async () => {
    const { terminal, statuses, last } = setup({ stableMs: 5_000 });
    terminal.start();
    for (let i = 0; i < 3; i += 1) {
      last().open();
      last().drop(1006);
      await vi.advanceTimersByTimeAsync(2_000);
    }
    last().open();
    last().drop(1006);
    expect(status(statuses)).toMatchObject({ kind: 'reconnecting', attempt: 4 });
  });
});

describe('pausing', () => {
  it('waits while the page is hidden or offline, and reconnects as soon as it is back', async () => {
    const { terminal, sockets, statuses, environment, last } = setup();
    terminal.start();
    last().open();
    last().output('abc');
    environment.state = 'hidden';
    last().drop(1006);
    await vi.advanceTimersByTimeAsync(60_000);
    expect(sockets).toHaveLength(1);
    expect(status(statuses)).toEqual({ kind: 'waiting', why: 'hidden' });

    environment.set('offline');
    await vi.advanceTimersByTimeAsync(60_000);
    expect(sockets).toHaveLength(1);
    expect(status(statuses)).toEqual({ kind: 'waiting', why: 'offline' });

    environment.set(undefined);
    expect(sockets).toHaveLength(2);
    expect(last().from).toBe(3);
  });

  it('does not connect at all while the browser starts offline', () => {
    const { terminal, sockets, statuses, environment } = setup();
    environment.state = 'offline';
    terminal.start();
    expect(sockets).toHaveLength(0);
    expect(status(statuses)).toEqual({ kind: 'waiting', why: 'offline' });
    environment.set(undefined);
    expect(sockets).toHaveLength(1);
  });

  it('stops listening to the page once stopped', () => {
    const { terminal, environment } = setup();
    terminal.start();
    expect(environment.listeners).toBe(1);
    terminal.stop();
    expect(environment.listeners).toBe(0);
  });

  it('holds while the view catches up, then resumes from what was received', () => {
    const { terminal, sockets, statuses, last } = setup();
    terminal.start();
    last().open();
    last().output('0123456789');
    terminal.hold();
    expect(sockets[0]?.closedWith?.code).toBe(1000);
    expect(status(statuses)).toEqual({ kind: 'waiting', why: 'busy' });
    // Late frames from the closed socket are ignored.
    sockets[0]?.output('late');
    expect(terminal.received).toBe(10);
    terminal.resume();
    expect(sockets).toHaveLength(2);
    expect(last().from).toBe(10);
  });
});

describe('keystrokes', () => {
  it('are binary frames of UTF-8', () => {
    const { terminal, last } = setup();
    terminal.start();
    last().open();
    expect(terminal.send('é\r')).toBe('sent');
    expect(terminal.send(Uint8Array.of(0x1b))).toBe('sent');
    expect(last().sent.every((d) => d instanceof Uint8Array)).toBe(true);
    expect(last().keys).toEqual([0xc3, 0xa9, 0x0d, 0x1b]);
  });

  it('wait while disconnected, up to 64 KiB, and are sent in order on reconnect', async () => {
    const { terminal, last } = setup();
    terminal.start();
    last().open();
    last().drop(1006);
    const kib = 'k'.repeat(1024);
    for (let i = 0; i < 63; i += 1) expect(terminal.send(kib)).toBe('queued');
    expect(terminal.send('x'.repeat(1024))).toBe('queued');
    expect(terminal.queued).toBe(INPUT_LIMIT);
    // Past the cap: refused, and the queue does not grow.
    expect(terminal.send('!')).toBe('refused');
    expect(terminal.queued).toBe(INPUT_LIMIT);
    await vi.advanceTimersByTimeAsync(1_000);
    last().open();
    const keys = last().keys;
    expect(keys).toHaveLength(INPUT_LIMIT);
    expect(String.fromCharCode(...keys.slice(-1024))).toBe('x'.repeat(1024));
    expect(terminal.queued).toBe(0);
    expect(terminal.send('after')).toBe('sent');
  });

  it('refuses more than 64 KiB at once, even when connected', () => {
    const { terminal, last } = setup();
    terminal.start();
    last().open();
    expect(terminal.send('p'.repeat(INPUT_LIMIT + 1))).toBe('refused');
    expect(last().sent).toHaveLength(0);
    expect(terminal.send('p'.repeat(INPUT_LIMIT))).toBe('sent');
  });

  it('refuses keystrokes a backed-up connection has not sent', () => {
    const { terminal, last } = setup();
    terminal.start();
    last().open();
    last().bufferedAmount = INPUT_LIMIT - 2;
    expect(terminal.send('ab')).toBe('sent');
    expect(terminal.send('abc')).toBe('refused');
  });

  it('are dropped, not sent, once the terminal has stopped', () => {
    const { terminal, last } = setup();
    terminal.start();
    last().drop(1006);
    terminal.send('queued');
    last().open();
    terminal.stop();
    expect(terminal.send('late')).toBe('closed');
  });
});

describe('resize', () => {
  it('is debounced, clamped to 1..=1000, and sent only when the size changed', async () => {
    const { terminal, last } = setup({ resizeMs: 100 });
    terminal.start();
    last().open();
    terminal.resize(100, 30);
    terminal.resize(110, 31);
    terminal.resize(120.4, 40);
    await vi.advanceTimersByTimeAsync(99);
    expect(last().controls).toEqual([]);
    await vi.advanceTimersByTimeAsync(1);
    expect(last().controls).toEqual([{ type: 'resize', cols: 120, rows: 40 }]);

    // The same size again: nothing.
    terminal.resize(120, 40);
    await vi.advanceTimersByTimeAsync(200);
    expect(last().controls).toHaveLength(1);

    terminal.resize(0, 5_000);
    await vi.advanceTimersByTimeAsync(200);
    expect(last().controls.at(-1)).toEqual({ type: 'resize', cols: 1, rows: 1000 });

    terminal.resize(Number.NaN, 10);
    terminal.resize(Number.POSITIVE_INFINITY, 10);
    await vi.advanceTimersByTimeAsync(200);
    expect(last().controls).toHaveLength(2);
  });

  it('while disconnected, goes in the next connection', async () => {
    const { terminal, last } = setup({ resizeMs: 100 });
    terminal.start();
    last().open();
    last().drop(1006);
    terminal.resize(132, 43);
    await vi.advanceTimersByTimeAsync(1_000);
    expect(last().query.get('cols')).toBe('132');
    expect(last().query.get('rows')).toBe('43');
    last().open();
    await vi.advanceTimersByTimeAsync(200);
    // The hub already has that size from the URL.
    expect(last().controls).toEqual([]);
  });

  it('sent when the size changed between connecting and opening', async () => {
    const { terminal, last } = setup({ resizeMs: 100 });
    terminal.start();
    terminal.resize(90, 30);
    await vi.advanceTimersByTimeAsync(100);
    last().open();
    expect(last().controls).toEqual([{ type: 'resize', cols: 90, rows: 30 }]);
  });
});

describe('stop', () => {
  it('closes, ignores the old socket, and never reconnects', async () => {
    const { terminal, sockets, output, statuses, last } = setup();
    terminal.start();
    const socket = last();
    socket.open();
    terminal.stop();
    expect(socket.closedWith?.code).toBe(1000);
    expect(status(statuses)).toEqual({ kind: 'closed' });
    socket.output('late');
    socket.drop(1006);
    await vi.advanceTimersByTimeAsync(60_000);
    expect(output).toEqual([]);
    expect(sockets).toHaveLength(1);
  });

  it('a factory that throws stops with a reason instead of looping', async () => {
    const statuses: TerminalStatus[] = [];
    const terminal = new TerminalSocket({
      sessionId: 'x',
      socket: () => {
        throw new SyntaxError('bad subprotocol');
      },
      size: { cols: 80, rows: 24 },
      onOutput: () => {},
      onStatus: (s) => statuses.push(s),
      environment: new FakeEnvironment(),
    });
    terminal.start();
    expect(status(statuses)?.kind).toBe('stopped');
  });
});

describe('the diagnosis of a refused upgrade', () => {
  const session = (patch: Partial<Session> = {}): Session => ({
    id: 'S1',
    engine: 'claude',
    native_id: 'n1',
    machine: 'M1',
    cwd: '/home/sam/work',
    state: 'working',
    started: 0,
    last_activity: 0,
    terminal: 'T1',
    ...patch,
  });
  const machine = (liveness: Machine['liveness']) => ({ id: 'M1', name: 'hpc-login', liveness }) as Machine;
  const api = (answer: () => Promise<Session>, machines: () => Promise<Machine[]> = async () => [machine('live')]) => ({
    session: answer,
    machines,
  });

  it('asks as the hub decides: the session, its machine, then its terminal', async () => {
    const gone = api(async () => {
      throw new ApiError('not_found', 'No session S1.', 404);
    });
    expect(await terminalDiagnosis(gone, 'S1')()).toEqual({ status: 404, message: 'This session no longer exists.' });

    const away = api(async () => session({ terminal: undefined }), async () => [machine('unverifiable')]);
    expect(await terminalDiagnosis(away, 'S1')()).toEqual({
      status: 503,
      message: 'hpc-login cannot be reached right now, so its terminal cannot be shown.',
    });
    const unreachable = api(async () => session({ state: 'unreachable' }));
    expect((await terminalDiagnosis(unreachable, 'S1')())?.status).toBe(503);
    const refused = api(async () => {
      throw new ApiError('unavailable', 'hpc-login cannot be reached right now.', 503);
    });
    expect(await terminalDiagnosis(refused, 'S1')()).toEqual({ status: 503, message: 'hpc-login cannot be reached right now.' });

    const none = api(async () => session({ terminal: undefined }));
    expect(await terminalDiagnosis(none, 'S1')()).toEqual({ status: 404, message: 'This session has no terminal.' });

    const token = api(async () => {
      throw new ApiError('unauthorized', 'No token.', 401);
    });
    expect((await terminalDiagnosis(token, 'S1')())?.status).toBe(401);
  });

  it('finds nothing to blame when the session is fine or the hub cannot be reached', async () => {
    expect(await terminalDiagnosis(api(async () => session()), 'S1')()).toBeUndefined();
    const offline = api(async () => {
      throw new ApiError('unavailable', 'Cannot reach the hub', 0);
    });
    expect(await terminalDiagnosis(offline, 'S1')()).toBeUndefined();
    // Without the machines, the session's own state still counts.
    const noMachines = api(
      async () => session(),
      async () => {
        throw new ApiError('internal', 'boom', 500);
      },
    );
    expect(await terminalDiagnosis(noMachines, 'S1')()).toBeUndefined();
  });
});