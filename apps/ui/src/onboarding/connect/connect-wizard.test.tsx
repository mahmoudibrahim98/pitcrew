// @vitest-environment happy-dom
// @vitest-environment-options {"url": "http://localhost:5173/"}
// The connect-a-remote wizard in the whole app, against a mocked gateway (Tauri's IPC, with the
// remote commands and prompts of desktop-gateway.md): probe, plan, Review showing the exact job
// script, connect with progress and an SSH prompt answered in the shell's dialog, setup of the
// fresh remote hub through its own gateway transport, and the new workspace opened. Then an
// expired plan going back to Review, a cancelled prompt failing the connect cleanly, a typed host
// starting with "-" refused, and a browser told it cannot.

import { cleanup, fireEvent, render, screen, within } from '@testing-library/react';
import { createMemoryHistory, RouterProvider } from '@tanstack/react-router';
import { afterEach, beforeEach, describe, expect, it, vi } from 'vitest';
import { WorkspacesProvider } from '../../data/desktop.tsx';
import { createGateway } from '../../data/gateway.ts';
import { FakeDesktop, freshDaemon, refuse, type FakeChannel } from '../../data/tests/fake-desktop.ts';
import type { GatewayWorkspace } from '../../data/workspaces.tsx';
import { createAppRouter } from '../../shell/routes.tsx';
import { initialShellState, useShell } from '../../shell/store.ts';
import { feature as onboarding } from '../index.ts';

const NEW: GatewayWorkspace = { id: '01JB000000000000000WSPNEW1', name: 'hpc-login', kind: 'remote', state: 'ready' };
const SCRIPT = [
  '#!/bin/bash',
  '#SBATCH --job-name=pitcrewd',
  '#SBATCH --partition=gpu',
  '#SBATCH --cpus-per-task=2',
  '#SBATCH --time=7-00:00:00',
  '',
  '\texec "$HOME/.pitcrew/bin/current/pitcrewd" serve  ',
  '',
].join('\n');
const SECRET = 'correct-horse-battery';
const PATIENCE = { timeout: 8_000 };

let desktop: FakeDesktop;

beforeEach(() => {
  desktop = new FakeDesktop().install();
  desktop.hosts = { hosts: ['hpc-login', 'build-box'] };
  desktop.probe = (host) => ({
    host,
    os: 'linux',
    arch: 'x86_64',
    slurm: { version: '23.02.7', defaultPartition: 'gpu', srunOverlap: true },
  });
  let plans = 0;
  desktop.plan = () => {
    plans += 1;
    return {
      plan: `plan-${plans}`,
      steps: [`Plan ${plans}: copy pitcrewd 0.4.0 to ~/.pitcrew`, 'Submit the job below'],
      jobScript: SCRIPT,
    };
  };
});

afterEach(() => {
  cleanup();
  desktop.uninstall();
  localStorage.clear();
  useShell.setState(initialShellState);
});

function renderApp(path = '/connect') {
  const router = createAppRouter([onboarding], { history: createMemoryHistory({ initialEntries: [path] }) });
  render(
    <WorkspacesProvider gateway={createGateway()}>
      <RouterProvider router={router} />
    </WorkspacesProvider>,
  );
  return router;
}

const heading = (name: string | RegExp) => screen.findByRole('heading', { level: 1, name }, PATIENCE);
const button = (name: string | RegExp) => screen.getByRole('button', { name });

/** Host, Probe and Launcher (SLURM, 2 CPUs), up to Review. */
async function toReview() {
  await heading('Connect a remote machine');
  fireEvent.click(await screen.findByRole('radio', { name: 'hpc-login' }));
  fireEvent.click(button('Continue'));

  await heading('Checking hpc-login');
  const probe = await screen.findByTestId('probe');
  expect(probe.textContent).toContain('linux x86_64');
  expect(probe.textContent).toContain('Not installed yet');
  expect(probe.textContent).toContain('23.02.7, default partition gpu');
  fireEvent.click(button('Continue'));

  await heading('How PitCrew runs on hpc-login');
  fireEvent.click(screen.getByRole('radio', { name: /As a SLURM job/ }));
  expect((screen.getByLabelText('Partition') as HTMLInputElement).value).toBe('gpu');
  fireEvent.change(screen.getByLabelText('CPUs'), { target: { value: '2' } });
  fireEvent.change(screen.getByLabelText('Time limit'), { target: { value: '7-00:00:00' } });
  fireEvent.click(button('Review the plan'));
  await heading('Review: connect hpc-login');
}

/** An add that asks for a password half-way, then registers the workspace (or fails if cancelled). */
function addAsking(): (plan: string, channel: FakeChannel) => Promise<unknown> {
  return async (_plan, channel) => {
    channel.send({ step: 'Copy pitcrewd 0.4.0', state: 'running' });
    const reply = desktop.nextReply();
    await desktop.prompt({ id: 'pw-1', host: 'hpc-login', kind: 'password', text: "sam@hpc-login's password: " });
    const answer = await reply;
    if (typeof answer.answer !== 'string') {
      channel.send({ step: 'Copy pitcrewd 0.4.0', state: 'failed', detail: 'Authentication cancelled.' });
      return refuse('unreachable', 'ssh: authentication cancelled');
    }
    channel.send({ step: 'Copy pitcrewd 0.4.0', state: 'done' });
    channel.send({ step: 'Submit the job', state: 'done', detail: 'Submitted batch job 4242' });
    desktop.daemons.set(NEW.id, fresh.daemon);
    await desktop.setWorkspaces([NEW]);
    channel.send({ step: 'Connect and pair', state: 'done' });
    return NEW;
  };
}

let fresh = freshDaemon(NEW.id);
beforeEach(() => {
  fresh = freshDaemon(NEW.id);
});

describe('connecting a remote machine', () => {
  it('probes, plans, shows the exact script, connects with a prompt, sets the hub up and opens it', async () => {
    desktop.add = addAsking();
    const router = renderApp();
    await toReview();

    expect(desktop.commands('gateway_remote_probe')).toEqual([{ host: 'hpc-login' }]);
    expect(desktop.commands('gateway_remote_plan')).toEqual([
      { req: { host: 'hpc-login', launcher: 'slurm', job: { partition: 'gpu', time: '7-00:00:00', cpus: 2 } } },
    ]);
    expect(screen.getByTestId('plan-steps').textContent).toContain('Plan 1: copy pitcrewd 0.4.0 to ~/.pitcrew');
    // Verbatim: tabs, trailing spaces and blank lines included.
    expect(screen.getByTestId('job-script').textContent).toBe(SCRIPT);
    expect(screen.getByText('Nothing changes on the remote until you press Connect.')).toBeTruthy();
    expect(desktop.commands('gateway_remote_add')).toEqual([]);

    fireEvent.click(button('Connect'));
    await heading('Connecting hpc-login');
    // SSH asks; the shell's dialog answers.
    const dialog = await screen.findByRole('dialog', { name: 'hpc-login asks for a password' }, PATIENCE);
    fireEvent.change(within(dialog).getByLabelText('Password'), { target: { value: SECRET } });
    fireEvent.click(within(dialog).getByRole('button', { name: 'Send' }));

    await heading('Set up hpc-login');
    expect(desktop.commands('gateway_remote_add').map((a) => a.plan)).toEqual(['plan-1']);
    expect(desktop.commands('gateway_prompt_reply')).toEqual([{ id: 'pw-1', answer: SECRET }]);

    // The fresh remote hub, through its own gateway transport.
    await screen.findByLabelText('Workspace name', undefined, PATIENCE);
    expect((screen.getByLabelText('Workspace name') as HTMLInputElement).value).toBe('hpc-login');
    expect((screen.getByLabelText('hpc-login’s name') as HTMLInputElement).value).toBe('hpc-login');
    fireEvent.change(screen.getByLabelText('Workspace name'), { target: { value: 'Cluster Lab' } });
    fireEvent.change(screen.getByLabelText('Your name'), { target: { value: 'Sam Rivera' } });
    fireEvent.click(button('Set up'));

    await heading('Connected');
    expect(fresh.setups).toEqual([
      { workspace_name: 'Cluster Lab', person: { name: 'Sam Rivera', handle: '@sam' }, machine_name: 'hpc-login' },
    ]);
    const setupRequest = desktop
      .commands('gateway_request')
      .map((a) => a.req as { workspace: string; method: string; path: string })
      .find((r) => r.method === 'POST');
    expect(setupRequest).toMatchObject({ workspace: NEW.id, path: '/v1/setup' });

    fireEvent.click(button('Open hpc-login'));
    await heading('Home');
    expect(router.state.location.pathname).toBe(`/w/${NEW.id}/home`);
    // The answer went to the gateway once, and is nowhere else.
    expect(JSON.stringify(useShell.getState())).not.toContain(SECRET);
    expect(document.documentElement.outerHTML).not.toContain(SECRET);
    expect(window.location.href).not.toContain(SECRET);
  }, 20_000);

  it('goes back to Review with a fresh plan when the gateway refuses an expired one', async () => {
    let adds = 0;
    desktop.add = (plan) => {
      adds += 1;
      if (adds === 1) return refuse('invalid', 'The plan has expired.');
      expect(plan).toBe('plan-2');
      return refuse('unreachable', 'stop here');
    };
    renderApp();
    await toReview();
    expect(screen.getByTestId('plan-steps').textContent).toContain('Plan 1');
    fireEvent.click(button('Connect'));

    await heading('Review: connect hpc-login');
    const notice = await screen.findByText(/The gateway refused that plan \(The plan has expired\.\)/);
    expect(notice.getAttribute('role')).toBe('status');
    // A fresh plan, shown before it can be submitted.
    await vi.waitFor(() => expect(screen.getByTestId('plan-steps').textContent).toContain('Plan 2'));
    expect(screen.getByTestId('job-script').textContent).toBe(SCRIPT);
    expect(desktop.commands('gateway_remote_add').map((a) => a.plan)).toEqual(['plan-1']);

    fireEvent.click(button('Connect'));
    await screen.findByRole('alert', undefined, PATIENCE);
    expect(desktop.commands('gateway_remote_add').map((a) => a.plan)).toEqual(['plan-1', 'plan-2']);
    expect(desktop.commands('gateway_remote_plan')).toHaveLength(2);
  });

  it('fails cleanly when the person cancels the prompt', async () => {
    desktop.add = addAsking();
    renderApp();
    await toReview();
    fireEvent.click(button('Connect'));
    const dialog = await screen.findByRole('dialog', { name: 'hpc-login asks for a password' }, PATIENCE);
    fireEvent.click(within(dialog).getByRole('button', { name: 'Cancel' }));

    const alert = await screen.findByRole('alert', undefined, PATIENCE);
    expect(alert.textContent).toBe('Connecting failed at “Copy pitcrewd 0.4.0”: Authentication cancelled.');
    expect(desktop.commands('gateway_prompt_reply')).toEqual([{ id: 'pw-1' }]);
    expect(screen.queryByRole('dialog')).toBeNull();
    expect(screen.queryByText(/^Working\./)).toBeNull();
    expect(within(screen.getByTestId('progress')).getByText('Failed')).toBeTruthy();

    // The plan was used: back to Review means a fresh one.
    fireEvent.click(button('Back to review'));
    await heading('Review: connect hpc-login');
    await vi.waitFor(() => expect(screen.getByTestId('plan-steps').textContent).toContain('Plan 2'));
  });

  it('refuses a typed host starting with "-", and probes one typed well', async () => {
    renderApp();
    await heading('Connect a remote machine');
    const typed = screen.getByLabelText('Or type a host');
    fireEvent.change(typed, { target: { value: '-oProxyCommand=sh' } });
    fireEvent.click(button('Continue'));
    expect((await screen.findByRole('alert')).textContent).toBe('A host cannot start with "-".');
    expect(typed.getAttribute('aria-invalid')).toBe('true');
    fireEvent.change(typed, { target: { value: 'hpc login' } });
    fireEvent.click(button('Continue'));
    expect((await screen.findByRole('alert')).textContent).toBe('A host cannot contain spaces or control characters.');
    expect(desktop.commands('gateway_remote_probe')).toEqual([]);

    fireEvent.change(typed, { target: { value: 'sam@server.example.org' } });
    fireEvent.click(button('Continue'));
    await heading('Checking sam@server.example.org');
    expect(desktop.commands('gateway_remote_probe')).toEqual([{ host: 'sam@server.example.org' }]);
  });

  it('offers SLURM only where the probe found it', async () => {
    desktop.probe = (host) => ({ host, os: 'linux', arch: 'aarch64', helper: { version: '0.4.0', running: true } });
    renderApp();
    await heading('Connect a remote machine');
    fireEvent.click(await screen.findByRole('radio', { name: 'build-box' }));
    fireEvent.click(button('Continue'));
    await heading('Checking build-box');
    expect((await screen.findByTestId('probe')).textContent).toContain('0.4.0, running');
    fireEvent.click(button('Continue'));
    await heading('How PitCrew runs on build-box');
    const slurm = screen.getByRole('radio', { name: /As a SLURM job/ }) as HTMLInputElement;
    expect(slurm.disabled).toBe(true);
    expect(screen.getByText('SLURM was not found on build-box.')).toBeTruthy();
    fireEvent.click(button('Review the plan'));
    await heading('Review: connect build-box');
    expect(desktop.commands('gateway_remote_plan')).toEqual([{ req: { host: 'build-box', launcher: 'tmux' } }]);
  });
});

describe('in a browser', () => {
  it('says connecting needs the desktop app', async () => {
    desktop.uninstall();
    const router = createAppRouter([onboarding], { history: createMemoryHistory({ initialEntries: ['/connect'] }) });
    render(<RouterProvider router={router} />);
    await screen.findByRole('heading', { level: 1, name: 'Connect a remote machine' }, PATIENCE);
    expect(screen.getByText(/needs the PitCrew desktop app/)).toBeTruthy();
    desktop.install();
  });
});
