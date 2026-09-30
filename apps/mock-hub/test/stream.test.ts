import assert from 'node:assert/strict';
import { describe, it } from 'node:test';
import type { RunningServer } from '../src/server.ts';
import type { ApiError, Dispatch, Event, StreamFrame, Task, TranscriptPage } from '../src/types.ts';
import { AGENT, DEVICE, ID, call, withServer } from './helpers.ts';
import { Refused, TestSocket } from './ws-client.ts';

type EventsFrame = Extract<StreamFrame, { type: 'events' }>;

function connect(server: RunningServer, token: string, since?: number): Promise<TestSocket> {
  const query = since === undefined ? '' : `?since=${since}`;
  return TestSocket.connect(`${server.url}/v1/stream${query}`, ['pitcrew.v1', `pitcrew.bearer.${token}`]);
}

async function refusal(promise: Promise<TestSocket>): Promise<Refused> {
  try {
    const socket = await promise;
    socket.destroy();
  } catch (error) {
    if (error instanceof Refused) {
      return error;
    }
    throw error;
  }
  throw new Error('the handshake was accepted');
}

/** Reads `events` frames (skipping pings) until `done` holds for everything received so far. */
async function eventsUntil(
  socket: TestSocket,
  done: (events: Event[]) => boolean,
  timeoutMs = 3000,
): Promise<Event[]> {
  const events: Event[] = [];
  const deadline = Date.now() + timeoutMs;
  while (!done(events)) {
    const frame = await socket.nextJson<StreamFrame>(Math.max(1, deadline - Date.now()));
    if (frame.type === 'events') {
      assert.equal(frame.to_rev - frame.from_rev + 1, frame.events.length);
      events.push(...frame.events);
    }
  }
  return events;
}

describe('event stream', () => {
  it('says hello, streams live events, and replays exactly what a reconnecting client missed', () =>
    withServer(async (server) => {
      const first = await connect(server, DEVICE);
      assert.equal(first.protocol, 'pitcrew.v1');
      assert.deepEqual(await first.nextJson(), { type: 'hello', rev: 15 });

      const moved = await call(server, 'POST', '/v1/tasks/PAP-2/move', {
        token: DEVICE,
        json: { to: 'in_progress' },
      });
      assert.equal(moved.status, 200);
      const live = await first.nextJson<EventsFrame>();
      assert.equal(live.type, 'events');
      assert.equal(live.from_rev, 16);
      assert.equal(live.to_rev, 16);
      assert.equal(live.events.length, 1);
      assert.deepEqual(live.events[0]?.body, {
        type: 'task_moved',
        data: {
          task: '01JB000000000000000TSK0002',
          from: 'todo',
          to: 'in_progress',
          mover: { kind: 'person' },
        },
      });
      first.close();
      assert.equal((await first.next()).type, 'close');

      // Two changes while the client is away.
      await call(server, 'POST', '/v1/tasks/PAP-2/move', { token: DEVICE, json: { to: 'review' } });
      await call(server, 'POST', '/v1/tasks/PAP-6/assign', { token: DEVICE, json: { assignee: ID.runner } });

      const second = await connect(server, DEVICE, live.to_rev);
      assert.deepEqual(await second.nextJson(), { type: 'hello', rev: 18 });
      const missed = await second.nextJson<EventsFrame>();
      assert.equal(missed.from_rev, 17);
      assert.equal(missed.to_rev, 18);
      assert.deepEqual(
        missed.events.map((e) => e.body.type),
        ['task_moved', 'task_assigned'],
      );
      // Nothing is sent twice once the batch timer fires.
      await second.expectQuiet(300);
      second.close();
    }));

  it('replays from any revision, including the fixture events', () =>
    withServer(async (server) => {
      const socket = await connect(server, DEVICE, 12);
      assert.deepEqual(await socket.nextJson(), { type: 'hello', rev: 15 });
      const replay = await socket.nextJson<EventsFrame>();
      assert.equal(replay.from_rev, 13);
      assert.equal(replay.to_rev, 15);
      assert.deepEqual(
        replay.events.map((e) => e.id),
        ['01JB000000000000000EVT0013', '01JB000000000000000EVT0014', '01JB000000000000000EVT0015'],
      );
      socket.close();
    }));

  it('refuses handshakes without the subprotocol, the token or a device scope', () =>
    withServer(async (server) => {
      const url = `${server.url}/v1/stream`;
      const noToken = await refusal(TestSocket.connect(url, ['pitcrew.v1']));
      assert.equal(noToken.status, 401);
      assert.equal((noToken.body as ApiError).code, 'unauthorized');
      const agent = await refusal(connect(server, AGENT));
      assert.equal(agent.status, 403);
      const noProtocol = await refusal(TestSocket.connect(url, [`pitcrew.bearer.${DEVICE}`]));
      assert.equal(noProtocol.status, 400);
      const badSince = await refusal(
        TestSocket.connect(`${url}?since=soon`, ['pitcrew.v1', `pitcrew.bearer.${DEVICE}`]),
      );
      assert.equal(badSince.status, 400);
    }));

  it('simulates a dispatched session: it starts, works, claims its task and answers a send', () =>
    withServer(
      async (server) => {
        const socket = await connect(server, DEVICE);
        await socket.nextJson();
        const dispatched = await call<Dispatch>(server, 'POST', '/v1/tasks/PAP-5/dispatch', {
          token: DEVICE,
          json: { agent: ID.runner, brief: 'Rerun seed 3 with lr 1e-4.' },
        });
        assert.equal(dispatched.status, 202);
        const session = dispatched.body.session;
        assert.equal(typeof session, 'string');

        const started = await eventsUntil(socket, (events) => events.some((e) => e.body.type === 'task_moved'));
        // PAP-5 had no assignee, so the dispatch assigns it to the agent first.
        assert.deepEqual(
          started.map((e) => e.body.type),
          ['task_assigned', 'dispatch_started', 'session_discovered', 'session_state_changed', 'task_moved'],
        );
        const [assigned, , discovered, working, claimed] = started;
        assert.deepEqual(assigned?.body, {
          type: 'task_assigned',
          data: { task: '01JB000000000000000TSK0005', assignee: ID.runner },
        });
        assert.equal(assigned?.author, ID.sam);
        assert.equal(discovered?.body.type === 'session_discovered' && discovered.body.data.session.state, 'starting');
        assert.equal(discovered?.author, ID.runner);
        assert.equal(discovered?.on_behalf_of, ID.sam);
        assert.deepEqual(working?.body.type === 'session_state_changed' && [working.body.data.from, working.body.data.to], [
          'starting',
          'working',
        ]);
        assert.deepEqual(claimed?.body.type === 'task_moved' && claimed.body.data.mover, {
          kind: 'agent',
          on_own_task: true,
        });

        const task = await call<Task>(server, 'GET', '/v1/tasks/PAP-5', { token: DEVICE });
        assert.deepEqual([task.body.assignee, task.body.status], [ID.runner, 'in_progress']);

        const sent = await call(server, 'POST', `/v1/sessions/${session}/send`, {
          token: DEVICE,
          json: { text: 'How is it going?' },
        });
        assert.equal(sent.status, 204);
        const turn = await eventsUntil(socket, (events) => events.some((e) => e.body.type === 'turn_ended'));
        const ended = turn.find((e) => e.body.type === 'turn_ended');
        const transcript = await call<TranscriptPage>(server, 'GET', `/v1/sessions/${session}/transcript`, {
          token: DEVICE,
        });
        assert.deepEqual(
          transcript.body.items.map((item) => item.kind),
          ['user_prompt', 'user_prompt', 'assistant_text', 'turn_ended'],
        );
        assert.equal(
          ended?.body.type === 'turn_ended' && ended.body.data.receipt.kind === 'transcript' && ended.body.data.receipt.offset,
          transcript.body.items.at(-1)?.offset,
        );
        socket.close();
      },
      { delays: { start: 20, reply: 20, end: 10 } },
    ));

  it('refuses to control sessions that ended or cannot be reached', () =>
    withServer(async (server) => {
      const ended = await call<ApiError>(server, 'POST', `/v1/sessions/${ID.ses6}/send`, {
        token: DEVICE,
        json: { text: 'hello?' },
      });
      assert.equal(ended.status, 409);
      const unreachable = await call<ApiError>(server, 'POST', `/v1/sessions/${ID.ses5}/interrupt`, {
        token: DEVICE,
      });
      assert.equal(unreachable.status, 503);
      assert.equal(unreachable.body.code, 'unavailable');
    }));
});
