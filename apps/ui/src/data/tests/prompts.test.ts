// @vitest-environment happy-dom
// The desktop registry's prompt queue: prompts from `gateway://prompt` queue in order, a
// `gateway://prompt-closed` withdraws one, a reply takes one off and goes to the gateway once, and
// none of it holds an answer.

import { afterEach, beforeEach, describe, expect, it, vi } from 'vitest';
import { Workspaces } from '../desktop.tsx';
import { createGateway } from '../gateway.ts';
import { FakeDesktop } from './fake-desktop.ts';

let desktop: FakeDesktop;
let registry: Workspaces;

beforeEach(() => {
  desktop = new FakeDesktop().install();
  registry = new Workspaces(createGateway());
  registry.start();
});

afterEach(() => {
  registry.stop();
  desktop.uninstall();
  vi.restoreAllMocks();
});

const ids = () => registry.prompts.getState().prompts.map((p) => p.id);

async function subscribed(): Promise<void> {
  await vi.waitFor(() => expect(desktop.calls.some((c) => c.cmd === 'gateway_workspaces')).toBe(true));
  // The prompt subscriptions are not awaited by start(); give them their turn.
  await new Promise((done) => setTimeout(done, 0));
}

describe('the prompt queue', () => {
  it('queues prompts from several hosts in order, and withdraws one on prompt-closed', async () => {
    await subscribed();
    await desktop.prompt({ id: 'a', host: 'hpc-login', kind: 'password', text: 'Password:' });
    await desktop.prompt({ id: 'b', host: 'build-box', kind: 'otp', text: 'Code:' });
    await desktop.prompt({ id: 'a', host: 'hpc-login', kind: 'password', text: 'Password again:' });
    expect(ids()).toEqual(['a', 'b']);
    expect(registry.prompts.getState().prompts[0]?.text).toBe('Password again:');

    await desktop.closePrompt('a');
    expect(ids()).toEqual(['b']);
    await desktop.closePrompt('nobody');
    expect(ids()).toEqual(['b']);
  });

  it('replies once, takes the prompt off, and ignores a second reply', async () => {
    await subscribed();
    await desktop.prompt({ id: 'a', host: 'hpc-login', kind: 'passphrase', text: 'Passphrase:' });
    const reply = desktop.nextReply();
    registry.replyPrompt('a', { answer: 'open sesame' });
    expect(await reply).toEqual({ id: 'a', answer: 'open sesame' });
    expect(ids()).toEqual([]);
    registry.replyPrompt('a', { answer: 'open sesame' });
    await new Promise((done) => setTimeout(done, 0));
    expect(desktop.commands('gateway_prompt_reply')).toHaveLength(1);
    // The registry keeps no answer anywhere it can be read back.
    expect(JSON.stringify(registry.prompts.getState())).not.toContain('open sesame');
  });

  it('logs nothing of a reply the gateway refuses', async () => {
    const warn = vi.spyOn(console, 'warn').mockImplementation(() => {});
    const error = vi.spyOn(console, 'error').mockImplementation(() => {});
    await subscribed();
    await desktop.prompt({ id: 'a', host: 'hpc-login', kind: 'password', text: 'Password:' });
    desktop.onReply = (args) => {
      throw new Error(`invalid args: ${String(args.answer)}`);
    };
    registry.replyPrompt('a', { answer: 'hunter2' });
    await vi.waitFor(() => expect(warn).toHaveBeenCalled());
    for (const call of [...warn.mock.calls, ...error.mock.calls]) expect(JSON.stringify(call)).not.toContain('hunter2');
  });

  it('empties the queue when stopped', async () => {
    await subscribed();
    await desktop.prompt({ id: 'a', host: 'hpc-login', kind: 'password', text: 'Password:' });
    registry.stop();
    expect(ids()).toEqual([]);
    await desktop.prompt({ id: 'b', host: 'hpc-login', kind: 'password', text: 'Password:' });
    expect(ids()).toEqual([]);
  });
});
