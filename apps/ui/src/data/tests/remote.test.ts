// @vitest-environment happy-dom
// The gateway's remote commands and prompt events, over a mocked `invoke`, `Channel` and `listen`
// (desktop-gateway.md, "Remote workspaces" and "Prompts"): the arguments each command takes, the
// checks on everything that comes back, and malformed payloads dropped or refused.

import { afterEach, beforeEach, describe, expect, it, vi } from 'vitest';
import { GatewayError } from '../errors.ts';
import { createRemoteGateway } from '../gateway.ts';
import type { GatewayPrompt, RemoteProgress } from '../remote.ts';
import { FakeDesktop, refuse } from './fake-desktop.ts';

let desktop: FakeDesktop;

beforeEach(() => {
  desktop = new FakeDesktop().install();
});

afterEach(() => {
  desktop.uninstall();
  vi.restoreAllMocks();
});

async function failure(promise: Promise<unknown>): Promise<unknown> {
  try {
    await promise;
  } catch (error) {
    return error;
  }
  throw new Error('expected a failure');
}

const NEW_WS = { id: '01JB000000000000000WSPNEW1', name: 'hpc-login', kind: 'remote', state: 'connecting' };

describe('the remote commands', () => {
  it('lists ssh hosts, dropping anything that is not a one-line name', async () => {
    desktop.hosts = { hosts: ['hpc-login', 42, '', 'build\nbox', 'build-box', 'hpc-login', null] };
    expect(await createRemoteGateway().sshHosts()).toEqual(['hpc-login', 'build-box']);
    desktop.hosts = { nothing: true };
    expect(await createRemoteGateway().sshHosts()).toEqual([]);
    expect(desktop.commands('gateway_ssh_hosts')).toEqual([{}, {}]);
  });

  it('probes a host by name, and refuses a malformed answer', async () => {
    desktop.probe = (host) => ({
      host,
      os: 'linux',
      arch: 'x86_64',
      helper: { version: '0.4.0', running: false },
      slurm: { version: '23.02.7', defaultPartition: 'gpu', srunOverlap: true },
    });
    const remote = createRemoteGateway();
    expect(await remote.remoteProbe('hpc-login')).toEqual({
      host: 'hpc-login',
      os: 'linux',
      arch: 'x86_64',
      helper: { version: '0.4.0', running: false },
      slurm: { version: '23.02.7', defaultPartition: 'gpu', srunOverlap: true },
    });
    expect(desktop.commands('gateway_remote_probe')).toEqual([{ host: 'hpc-login' }]);

    desktop.probe = () => ({ host: 'hpc-login', os: 'linux' });
    const error = await failure(remote.remoteProbe('hpc-login'));
    expect(error).toBeInstanceOf(GatewayError);
    expect((error as GatewayError).gateway).toBe('internal');

    desktop.probe = () => ({ host: 'h', os: 'linux', arch: 'x86_64', slurm: { version: 23 } });
    expect(await failure(remote.remoteProbe('h'))).toBeInstanceOf(GatewayError);
  });

  it('passes the gateway refusal on as a GatewayError', async () => {
    desktop.probe = () => refuse('unreachable', 'ssh: connect to host hpc-login port 22: timed out');
    const error = await failure(createRemoteGateway().remoteProbe('hpc-login'));
    expect(error).toMatchObject({ gateway: 'unreachable', code: 'unavailable', message: 'ssh: connect to host hpc-login port 22: timed out' });
  });

  it('asks for a plan with one argument, req, and keeps the job script verbatim', async () => {
    const script = '#!/bin/bash\n#SBATCH --partition=gpu\n#SBATCH --time=7-00:00:00\n\texec pitcrewd serve  \n';
    desktop.plan = () => ({ plan: 'plan-7', steps: ['Copy pitcrewd 0.4.0 to ~/.pitcrew', 'Submit the job below'], jobScript: script });
    const request = { host: 'hpc-login', launcher: 'slurm' as const, site: 'example-site', job: { partition: 'gpu', cpus: 2 } };
    const plan = await createRemoteGateway().remotePlan(request);
    expect(plan).toEqual({ plan: 'plan-7', steps: ['Copy pitcrewd 0.4.0 to ~/.pitcrew', 'Submit the job below'], jobScript: script });
    expect(desktop.commands('gateway_remote_plan')).toEqual([{ req: request }]);

    desktop.plan = () => ({ plan: 'plan-8', steps: 'all of them' });
    expect(await failure(createRemoteGateway().remotePlan(request))).toBeInstanceOf(GatewayError);
    desktop.plan = () => ({ plan: '', steps: [] });
    expect(await failure(createRemoteGateway().remotePlan(request))).toBeInstanceOf(GatewayError);
  });

  it('adds a plan, with its progress on a channel, dropping malformed messages', async () => {
    const warn = vi.spyOn(console, 'warn').mockImplementation(() => {});
    desktop.add = async (plan, channel) => {
      expect(plan).toBe('plan-7');
      channel.send({ step: 'Copy pitcrewd', state: 'running' });
      channel.send({ step: 'Copy pitcrewd', state: 'sideways' });
      channel.send('not a message');
      channel.send({ step: 'Copy pitcrewd', state: 'done', detail: 'sha256 \u0007ok' });
      channel.send({ step: 'Connect', state: 'done' });
      return NEW_WS;
    };
    const seen: RemoteProgress[] = [];
    const workspace = await createRemoteGateway().remoteAdd('plan-7', (p) => seen.push(p));
    expect(workspace).toEqual(NEW_WS);
    expect(seen).toEqual([
      { step: 'Copy pitcrewd', state: 'running' },
      { step: 'Copy pitcrewd', state: 'done', detail: 'sha256 ok' },
      { step: 'Connect', state: 'done' },
    ]);
    expect(warn).toHaveBeenCalledTimes(1);
    const [args] = desktop.commands('gateway_remote_add');
    expect(Object.keys(args ?? {}).sort()).toEqual(['events', 'plan']);
  });

  it('refuses an add whose answer is not a workspace, and passes on an expired plan as invalid', async () => {
    desktop.add = () => Promise.resolve({ id: 'x', name: 'x', kind: 'remote', state: 'happy' });
    expect(await failure(createRemoteGateway().remoteAdd('p', () => {}))).toMatchObject({ gateway: 'internal' });
    desktop.add = () => refuse('invalid', 'The plan has expired.');
    expect(await failure(createRemoteGateway().remoteAdd('p', () => {}))).toMatchObject({ gateway: 'invalid', message: 'The plan has expired.' });
  });

  it('removes a workspace, saying whether to stop its helper', async () => {
    await createRemoteGateway().workspaceRemove('01JB000000000000000WSPNEW1', true);
    await createRemoteGateway().workspaceRemove('01JB000000000000000WSPNEW1', false);
    expect(desktop.commands('gateway_workspace_remove')).toEqual([
      { workspace: '01JB000000000000000WSPNEW1', stopHelper: true },
      { workspace: '01JB000000000000000WSPNEW1', stopHelper: false },
    ]);
  });
});

describe('prompts', () => {
  it('delivers checked prompts and drops malformed ones', async () => {
    const warn = vi.spyOn(console, 'warn').mockImplementation(() => {});
    const remote = createRemoteGateway();
    const prompts: GatewayPrompt[] = [];
    const closed: string[] = [];
    const unlisten = await remote.onPrompt((p) => prompts.push(p));
    const unlistenClosed = await remote.onPromptClosed((id) => closed.push(id));

    await desktop.prompt({ id: 'p1', host: 'hpc-login', kind: 'password', text: "sam@hpc-login's password: " });
    await desktop.prompt({ id: 'p2', host: 'hpc-login', kind: 'telepathy', text: 'Think of it.' });
    await desktop.prompt({ id: 'p3', host: 'hpc-login', kind: 'otp' });
    await desktop.prompt({ id: '', host: 'hpc-login', kind: 'otp', text: 'Code:' });
    await desktop.prompt('p4');
    await desktop.prompt({ id: 'p5', host: 'hpc-login', kind: 'host_key', text: 'Accept?\u001b[31m', fingerprint: 'SHA256:abc' });
    await desktop.closePrompt('p1');
    await desktop.closePrompt(7);

    expect(prompts).toEqual([
      { id: 'p1', host: 'hpc-login', kind: 'password', text: "sam@hpc-login's password: " },
      { id: 'p5', host: 'hpc-login', kind: 'host_key', text: 'Accept?[31m', fingerprint: 'SHA256:abc' },
    ]);
    expect(closed).toEqual(['p1']);
    expect(warn).toHaveBeenCalledTimes(4);
    unlisten();
    unlistenClosed();
    await desktop.prompt({ id: 'p6', host: 'hpc-login', kind: 'password', text: 'Again?' });
    expect(prompts).toHaveLength(2);
  });

  it('replies with exactly the answer, the acceptance, or neither', async () => {
    const remote = createRemoteGateway();
    await remote.replyPrompt('p1', { answer: 'correct horse' });
    await remote.replyPrompt('p2', { accept: false });
    await remote.replyPrompt('p3', {});
    expect(desktop.commands('gateway_prompt_reply')).toEqual([
      { id: 'p1', answer: 'correct horse' },
      { id: 'p2', accept: false },
      { id: 'p3' },
    ]);
  });
});
