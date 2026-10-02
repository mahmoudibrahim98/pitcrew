// The desktop registry's list and its subscriptions, over a stub gateway: every event (workspaces,
// navigate, prompts) is followed before the first `gateway_workspaces` read, since the gateway
// holds a launch-time deep link and any prompt raised before the page listens until that read;
// one validator for every workspace; and a payload that is not a list never empties a known list.

import { afterEach, describe, expect, it, vi } from 'vitest';
import { Workspaces } from '../desktop.tsx';
import type { GatewayPrompt, RemoteGateway } from '../remote.ts';
import type { Gateway, GatewayWorkspace } from '../workspaces.tsx';

const A: GatewayWorkspace = { id: 'WS-A', name: 'Alpha Lab', kind: 'local', state: 'ready' };
const B: GatewayWorkspace = { id: 'WS-B', name: 'hpc-login', kind: 'remote', state: 'unreachable', detail: 'Sign-in cancelled.' };

interface Stub {
  gateway: Gateway;
  order: string[];
  emitList(list: unknown): void;
  emitPrompt(prompt: GatewayPrompt): void;
  /** Lets `onPrompt` resolve: the subscription is held until then. */
  releasePrompt(): void;
  answer(list: unknown): void;
}

function stub(): Stub {
  const order: string[] = [];
  let listListener: ((list: GatewayWorkspace[]) => void) | undefined;
  let promptListener: ((prompt: GatewayPrompt) => void) | undefined;
  let releasePrompt: () => void = () => {};
  let answer: (list: unknown) => void = () => {};
  const promptHeld = new Promise<void>((done) => (releasePrompt = done));
  const no = () => Promise.reject(new Error('not in this test'));
  const remote: RemoteGateway = {
    sshHosts: no,
    remoteProbe: no,
    remotePlan: no,
    remoteAdd: no,
    workspaceRemove: no,
    workspaceRetry: no,
    remoteCancel: no,
    onPrompt: async (listener) => {
      await promptHeld;
      order.push('listen prompt');
      promptListener = listener;
      return () => (promptListener = undefined);
    },
    onPromptClosed: async () => {
      order.push('listen prompt-closed');
      return () => {};
    },
    replyPrompt: () => Promise.resolve(),
  };
  const gateway: Gateway = {
    workspaces: () => {
      order.push('read workspaces');
      return new Promise((resolve) => (answer = resolve as (list: unknown) => void));
    },
    onWorkspaces: async (listener) => {
      order.push('listen workspaces');
      listListener = listener;
      return () => (listListener = undefined);
    },
    onNavigate: async () => {
      order.push('listen navigate');
      return () => {};
    },
    transport: () => {
      throw new Error('no transport in this test');
    },
    remote,
  };
  return {
    gateway,
    order,
    emitList: (list) => listListener?.(list as GatewayWorkspace[]),
    emitPrompt: (prompt) => promptListener?.(prompt),
    releasePrompt: () => releasePrompt(),
    answer: (list) => answer(list),
  };
}

let registry: Workspaces | undefined;

afterEach(() => {
  registry?.stop();
  vi.restoreAllMocks();
});

describe('the workspace registry', () => {
  it('follows every event, prompts included, before it first reads the list', async () => {
    const s = stub();
    registry = new Workspaces(s.gateway);
    registry.start();
    await new Promise((done) => setTimeout(done, 10));
    // The prompt subscription is still out: no read yet.
    expect(s.order).not.toContain('read workspaces');
    s.releasePrompt();
    await vi.waitFor(() => expect(s.order).toContain('read workspaces'));
    expect(s.order.indexOf('listen prompt')).toBeLessThan(s.order.indexOf('read workspaces'));
    expect(s.order.indexOf('listen prompt-closed')).toBeLessThan(s.order.indexOf('read workspaces'));
    expect(s.order.indexOf('listen workspaces')).toBeLessThan(s.order.indexOf('read workspaces'));
    expect(s.order.indexOf('listen navigate')).toBeLessThan(s.order.indexOf('read workspaces'));

    // A prompt the gateway held until that read reaches the queue.
    s.emitPrompt({ id: 'p1', host: 'hpc-login', kind: 'password', text: 'Password:' });
    expect(registry.prompts.getState().prompts.map((p) => p.id)).toEqual(['p1']);
  });

  it('checks each workspace with one validator, keeping `detail` only as a string', async () => {
    const s = stub();
    s.releasePrompt();
    registry = new Workspaces(s.gateway);
    registry.start();
    await vi.waitFor(() => expect(s.order).toContain('read workspaces'));
    s.answer([
      A,
      B,
      { id: 'WS-C', name: 'No kind', state: 'ready' },
      { id: 'WS-D', name: 'Odd detail', kind: 'remote', state: 'connecting', detail: { html: '<b>x</b>' } },
      'WS-E',
    ]);
    await vi.waitFor(() => expect(registry?.store.getState().list).toBeDefined());
    expect(registry.store.getState().list).toEqual([A, B, { id: 'WS-D', name: 'Odd detail', kind: 'remote', state: 'connecting' }]);
  });

  it('keeps the known list when an event is not a list, and retries a read that is not one', async () => {
    vi.spyOn(console, 'warn').mockImplementation(() => {});
    const s = stub();
    s.releasePrompt();
    registry = new Workspaces(s.gateway, { listBackoff: { initialMs: 10, maxMs: 10 } });
    registry.start();
    await vi.waitFor(() => expect(s.order).toContain('read workspaces'));
    s.answer({ workspaces: [A] });
    await vi.waitFor(() => expect(registry?.store.getState().error).toMatch(/not a list/));
    expect(registry.store.getState().list).toBeUndefined();
    // Read again, with back-off, until it is a list.
    await vi.waitFor(() => expect(s.order.filter((o) => o === 'read workspaces')).toHaveLength(2));
    s.answer([A, B]);
    await vi.waitFor(() => expect(registry?.store.getState().list).toEqual([A, B]));

    s.emitList(null);
    s.emitList({ list: [] });
    expect(registry.store.getState().list).toEqual([A, B]);
    s.emitList([A]);
    expect(registry.store.getState().list).toEqual([A]);
  });
});
