import { afterEach, beforeEach, describe, expect, it, vi } from 'vitest';
import { createApi } from '../src/data/api.ts';
import { StreamClient, streamUrl, type StreamStatus } from '../src/data/stream.ts';
import { browserTransport } from '../src/data/transport.ts';
import type { Event, Task, TaskStatus } from '../src/data/types.ts';
import { DEVICE_TOKEN, fakeSockets, recordingSocket, startServer, type RunningServer } from './helpers.ts';

const NEXT: Record<TaskStatus, TaskStatus> = {
  backlog: 'todo',
  todo: 'in_progress',
  in_progress: 'review',
  review: 'done',
  done: 'todo',
  canceled: 'todo',
};

function taskMoved(id: string): Event {
  return {
    id,
    at: 0,
    workspace: 'W',
    author: 'M',
    body: { type: 'task_moved', data: { task: 'T', from: 'todo', to: 'review', mover: { kind: 'person' } } },
  };
}

describe('stream client against the mock hub', () => {
  let hub: RunningServer;
  const clients: StreamClient[] = [];

  beforeEach(async () => {
    hub = await startServer({ port: 0 });
  });

  afterEach(async () => {
    for (const client of clients.splice(0)) client.stop();
    await hub.close();
  });

  function client(options: { since?: number; onReset?: (rev: number) => void } = {}) {
    const received: Event[] = [];
    const statuses: StreamStatus[] = [];
    const socket = recordingSocket();
    const stream = new StreamClient({
      transport: browserTransport({ baseUrl: hub.url, token: DEVICE_TOKEN, socket: socket.factory }),
      since: options.since,
      onEvents: (events) => received.push(...events),
      onReset: options.onReset ?? (() => {}),
      onStatus: (status) => statuses.push(status),
      backoff: { initialMs: 10, maxMs: 50 },
    });
    clients.push(stream);
    return { stream, received, statuses, urls: socket.urls };
  }

  async function moveSomeTask(): Promise<Task> {
    const api = createApi({ baseUrl: hub.url, token: DEVICE_TOKEN });
    const [task] = await api.tasks({ status: ['todo'] });
    if (task === undefined) throw new Error('the fixture has no todo task');
    return api.moveTask(task.id, NEXT[task.status]);
  }

  it('starts from hello, then resumes from the last to_rev after a disconnect', async () => {
    const { stream, received, statuses, urls } = client();
    stream.start();
    await vi.waitFor(() => expect(stream.status).toBe('live'));
    const start = stream.rev ?? -1;
    expect(start).toBeGreaterThan(0);
    expect(urls[0]).not.toContain('since=');

    const first = await moveSomeTask();
    await vi.waitFor(() => expect(stream.rev).toBe(start + 1));
    expect(received.map((e) => e.body.type)).toEqual(['task_moved']);
    expect(received[0]?.body).toMatchObject({ type: 'task_moved', data: { task: first.id } });

    // Missed while disconnected.
    stream.stop();
    const second = await moveSomeTask();

    stream.start();
    await vi.waitFor(() => expect(stream.rev).toBe(start + 2));
    expect(urls[1]).toContain(`since=${start + 1}`);
    expect(received).toHaveLength(2);
    expect(received[1]?.body).toMatchObject({ type: 'task_moved', data: { task: second.id } });
    expect(statuses).toEqual(['connecting', 'live', 'stopped', 'connecting', 'live']);
  });

  it('resets when since is ahead of hello.rev', async () => {
    const resets: number[] = [];
    const { stream, received } = client({ since: 10_000, onReset: (rev) => resets.push(rev) });
    stream.start();
    await vi.waitFor(() => expect(resets).toHaveLength(1));
    expect(resets[0]).toBeLessThan(10_000);
    expect(stream.rev).toBe(resets[0]);
    expect(received).toEqual([]);

    // Live events after the reset arrive as usual.
    await moveSomeTask();
    await vi.waitFor(() => expect(received).toHaveLength(1));
  });

  it('resets after the hub restarts with a shorter history, and reconnects by itself', async () => {
    const resets: number[] = [];
    const { stream } = client({ onReset: (rev) => resets.push(rev) });
    stream.start();
    await vi.waitFor(() => expect(stream.status).toBe('live'));
    const start = stream.rev ?? -1;
    await moveSomeTask();
    await vi.waitFor(() => expect(stream.rev).toBe(start + 1));

    const port = hub.port;
    await hub.close();
    await vi.waitFor(() => expect(stream.status).toBe('reconnecting'));
    hub = await startServer({ port });

    await vi.waitFor(() => expect(resets).toEqual([start]));
    expect(stream.rev).toBe(start);
    expect(stream.status).toBe('live');
  });
});

describe('stream client timing', () => {
  beforeEach(() => {
    vi.useFakeTimers();
  });

  afterEach(() => {
    vi.useRealTimers();
  });

  function setup(since?: number) {
    const { factory, sockets } = fakeSockets();
    const received: Event[] = [];
    const stream = new StreamClient({
      transport: browserTransport({ baseUrl: 'http://127.0.0.1:47317', token: DEVICE_TOKEN, socket: factory }),
      since,
      onEvents: (events) => received.push(...events),
      onReset: () => {},
      backoff: { initialMs: 100, maxMs: 1000 },
      random: () => 1,
    });
    return { stream, sockets, received };
  }

  it('authenticates with subprotocols, never the query string', () => {
    const { stream, sockets } = setup(7);
    stream.start();
    expect(sockets[0]?.protocols).toEqual(['pitcrew.v1', `pitcrew.bearer.${DEVICE_TOKEN}`]);
    expect(sockets[0]?.url).toBe('ws://127.0.0.1:47317/v1/stream?since=7');
    expect(sockets[0]?.url).not.toContain(DEVICE_TOKEN);
    stream.stop();
  });

  it('uses wss for an https base, and keeps a path prefix', () => {
    expect(streamUrl('https://hub.localhost', undefined)).toBe('wss://hub.localhost/v1/stream');
    expect(streamUrl('http://127.0.0.1:9000/hub', 3)).toBe('ws://127.0.0.1:9000/hub/v1/stream?since=3');
    expect(streamUrl('http://127.0.0.1:9000/hub/', undefined)).toBe('ws://127.0.0.1:9000/hub/v1/stream');
  });

  it('resets on a gap in revisions instead of skipping ahead', () => {
    const resets: number[] = [];
    const { factory, sockets } = fakeSockets();
    const received: Event[] = [];
    const stream = new StreamClient({
      transport: browserTransport({ baseUrl: 'http://127.0.0.1:47317', socket: factory }),
      since: 5,
      onEvents: (events) => received.push(...events),
      onReset: (rev) => resets.push(rev),
    });
    stream.start();
    sockets[0]?.send({ type: 'hello', rev: 9 });
    sockets[0]?.send({ type: 'events', from_rev: 8, to_rev: 9, events: [taskMoved('E8'), taskMoved('E9')] });
    expect(resets).toEqual([9]);
    expect(received).toEqual([]);
    expect(stream.rev).toBe(9);
    sockets[0]?.send({ type: 'events', from_rev: 10, to_rev: 10, events: [taskMoved('E10')] });
    expect(received.map((e) => e.id)).toEqual(['E10']);
    stream.stop();
  });

  it('resets when a restarted hub has another log, even if its rev is past ours', () => {
    const resets: number[] = [];
    const { factory, sockets } = fakeSockets();
    const received: Event[] = [];
    const stream = new StreamClient({
      transport: browserTransport({ baseUrl: 'http://127.0.0.1:47317', socket: factory }),
      onEvents: (events) => received.push(...events),
      onReset: (rev) => resets.push(rev),
      backoff: { initialMs: 100, maxMs: 100 },
      random: () => 1,
    });
    stream.start();
    sockets[0]?.send({ type: 'hello', rev: 5, log: 'LOG-A' });
    sockets[0]?.send({ type: 'events', from_rev: 6, to_rev: 7, events: [taskMoved('E6'), taskMoved('E7')] });

    // Same log after a reconnect: resume, no reset.
    sockets[0]?.drop();
    vi.advanceTimersByTime(100);
    expect(sockets[1]?.url).toContain('since=7');
    sockets[1]?.send({ type: 'hello', rev: 7, log: 'LOG-A' });
    expect(resets).toEqual([]);

    // Another log whose rev (30) is already past our since (7).
    sockets[1]?.drop();
    vi.advanceTimersByTime(100);
    sockets[2]?.send({ type: 'hello', rev: 30, log: 'LOG-B' });
    // The hub replays "missed" events from the new log; they must not be applied.
    sockets[2]?.send({ type: 'events', from_rev: 8, to_rev: 30, events: [taskMoved('X8')] });
    expect(resets).toEqual([30]);
    expect(stream.rev).toBe(30);
    expect(received.map((e) => e.id)).toEqual(['E6', 'E7']);
    stream.stop();
  });

  it('keeps backing off when a server accepts and then drops the connection', () => {
    const { stream, sockets } = setup();
    stream.start();
    const delays: number[] = [];
    for (let attempt = 0; attempt < 4; attempt++) {
      const count = sockets.length;
      sockets[count - 1]?.send({ type: 'hello', rev: 1 });
      sockets[count - 1]?.drop();
      let waited = 0;
      while (sockets.length === count) {
        vi.advanceTimersByTime(50);
        waited += 50;
      }
      delays.push(waited);
    }
    expect(delays).toEqual([100, 200, 400, 800]);
    stream.stop();
  });

  it('reconnects after 60 s of silence, resuming from its revision', () => {
    const { stream, sockets } = setup();
    stream.start();
    sockets[0]?.send({ type: 'hello', rev: 5 });
    sockets[0]?.send({ type: 'events', from_rev: 6, to_rev: 6, events: [taskMoved('E6')] });

    vi.advanceTimersByTime(59_999);
    expect(sockets).toHaveLength(1);
    sockets[0]?.send({ type: 'ping', at: 0 }); // any frame resets the timer
    vi.advanceTimersByTime(59_999);
    expect(sockets).toHaveLength(1);

    vi.advanceTimersByTime(1);
    expect(sockets[0]?.closed).toBe(true);
    expect(stream.status).toBe('reconnecting');
    vi.advanceTimersByTime(100);
    expect(sockets).toHaveLength(2);
    expect(sockets[1]?.url).toContain('since=6');
    stream.stop();
  });

  it('backs off exponentially up to the ceiling, and starts over once a connection is stable', () => {
    const { stream, sockets } = setup();
    stream.start();
    const delays: number[] = [];
    for (let attempt = 0; attempt < 6; attempt++) {
      const count = sockets.length;
      sockets[count - 1]?.drop();
      let waited = 0;
      while (sockets.length === count) {
        vi.advanceTimersByTime(50);
        waited += 50;
      }
      delays.push(waited);
    }
    expect(delays).toEqual([100, 200, 400, 800, 1000, 1000]);

    sockets.at(-1)?.send({ type: 'hello', rev: 1 });
    vi.advanceTimersByTime(5_000);
    sockets.at(-1)?.drop();
    vi.advanceTimersByTime(100);
    expect(sockets).toHaveLength(8);
    stream.stop();
  });

  it('retryNow() reconnects at once: now while waiting, or as soon as an open in flight fails', () => {
    const { stream, sockets } = setup();
    stream.start();
    // Waiting to reconnect: now.
    sockets[0]?.drop();
    expect(sockets).toHaveLength(1);
    stream.retryNow();
    expect(sockets).toHaveLength(2);

    // The hub is back while that connection is still being opened, and it then fails anyway.
    stream.retryNow();
    sockets[1]?.drop();
    vi.advanceTimersByTime(0);
    expect(sockets).toHaveLength(3);

    // Only once: the next failure waits for the back-off again (the third: 400 ms).
    sockets[2]?.drop();
    vi.advanceTimersByTime(399);
    expect(sockets).toHaveLength(3);
    vi.advanceTimersByTime(1);
    expect(sockets).toHaveLength(4);

    // Live, or stopped: nothing to do.
    sockets[3]?.send({ type: 'hello', rev: 1 });
    stream.retryNow();
    expect(sockets).toHaveLength(4);
    stream.stop();
    stream.retryNow();
    expect(sockets).toHaveLength(4);
  });

  it('skips events it has already seen', () => {
    const { stream, sockets, received } = setup(5);
    stream.start();
    sockets[0]?.send({ type: 'hello', rev: 7 });
    sockets[0]?.send({ type: 'events', from_rev: 6, to_rev: 7, events: [taskMoved('E6'), taskMoved('E7')] });
    sockets[0]?.send({ type: 'events', from_rev: 7, to_rev: 8, events: [taskMoved('E7'), taskMoved('E8')] });
    sockets[0]?.send({ type: 'events', from_rev: 8, to_rev: 8, events: [taskMoved('E8')] });
    expect(received.map((e) => e.id)).toEqual(['E6', 'E7', 'E8']);
    expect(stream.rev).toBe(8);
    stream.stop();
  });

  it('ignores callbacks from a socket it has let go', () => {
    const { stream, sockets, received } = setup();
    stream.start();
    const old = sockets[0];
    old?.send({ type: 'hello', rev: 1 });
    stream.stop();
    old?.send({ type: 'events', from_rev: 2, to_rev: 2, events: [taskMoved('E2')] });
    old?.drop();
    vi.advanceTimersByTime(10_000);
    expect(received).toEqual([]);
    expect(sockets).toHaveLength(1);
    expect(stream.status).toBe('stopped');
  });
});
