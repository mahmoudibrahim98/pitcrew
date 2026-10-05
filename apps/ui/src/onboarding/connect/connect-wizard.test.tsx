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
import { StrictMode } from 'react';
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
      steps: stepsOf(`plan-${plans}`),
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

/**
 * The whole app, in StrictMode as `main.tsx` runs it: effects run twice in development, and no
 * remote call may run twice because of it.
 */
function renderApp(path = '/connect') {
  const router = createAppRouter([onboarding], { history: createMemoryHistory({ initialEntries: [path] }) });
  render(
    <StrictMode>
      <WorkspacesProvider gateway={createGateway()}>
        <RouterProvider router={router} />
      </WorkspacesProvider>
    </StrictMode>,
  );
  return router;
}

const heading = (name: string | RegExp) => screen.findByRole('heading', { level: 1, name }, PATIENCE);
const button = (name: string | RegExp) => screen.getByRole('button', { name });

it('selects a stopped WSL2 distro and reviews a WSL plan without SSH prompts', async () => {
  const distro = "Lab 'quoted' distro";
  desktop.wsl = { available: true, distros: [
    { name: distro, default: true, running: false, version: 2 },
    { name: 'Legacy distro', default: false, running: false, version: 1 },
  ] };
  desktop.probe = () => ({ host: distro, os: 'linux', arch: 'x86_64' });
  desktop.plan = () => ({ plan: 'wsl-plan', steps: ['Copy the Linux helper', 'Connect through WSL stdio'] });
  renderApp();
  await heading('Connect a remote machine');
  expect((await screen.findByRole('radio', { name: /Legacy distro/ }) as HTMLInputElement).disabled).toBe(true);
  fireEvent.click(await screen.findByRole('radio', { name: /Lab 'quoted' distro/ }));
  fireEvent.click(button('Continue'));
  await screen.findByTestId('probe');
  fireEvent.click(button('Continue'));
  await heading(`How PitCrew runs on ${distro}`);
  fireEvent.click(screen.getByRole('radio', { name: /Directly/ }));
  fireEvent.click(button('Review the plan'));
  await heading(`Review: connect ${distro}`);
  expect(await screen.findByText('Connect through WSL stdio')).toBeTruthy();
  const target = { kind: 'wsl', distro };
  expect(desktop.commands('gateway_remote_probe')).toEqual([{ host: '', target }]);
  expect(desktop.commands('gateway_remote_plan')).toEqual([{ req: { host: '', target, launcher: 'direct' } }]);
});

it('offers no WSL choice when WSL lists no distribution', async () => {
  desktop.wsl = { available: true, distros: [] };
  renderApp();
  await heading('Connect a remote machine');
  await screen.findByRole('radio', { name: 'hpc-login' }, PATIENCE);
  await vi.waitFor(() => expect(desktop.commands('gateway_wsl_distros').length).toBeGreaterThan(0));
  await new Promise((resolve) => setTimeout(resolve, 50));
  expect(screen.queryByText('A WSL distro on this computer')).toBeNull();
});

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

/** A plan's steps, as the gateway names them; the plan's id shows in the first. */
function stepsOf(plan: string): string[] {
  return [`Copy pitcrewd 0.4.0 to ~/.pitcrew (${plan})`, 'Submit the job below', 'Connect and pair'];
}

/**
 * An add that asks for a password half-way, then registers the workspace (or fails if cancelled).
 * Each message's step is one of the plan's steps; the last is the whole add's, `add`.
 */
function addAsking(): (plan: string, channel: FakeChannel) => Promise<unknown> {
  return async (plan, channel) => {
    const [copy = '', submit = '', connect = ''] = stepsOf(plan);
    channel.send({ step: copy, state: 'running' });
    channel.send({ step: copy, state: 'running', detail: '40% sent' });
    const reply = desktop.nextReply();
    await desktop.prompt({ id: 'pw-1', host: 'hpc-login', kind: 'password', text: "sam@hpc-login's password: " });
    const answer = await reply;
    if (typeof answer.answer !== 'string') {
      channel.send({ step: copy, state: 'failed', detail: 'Authentication cancelled.' });
      channel.send({ step: 'add', state: 'failed', detail: 'ssh: authentication cancelled' });
      return refuse('unreachable', 'ssh: authentication cancelled');
    }
    channel.send({ step: copy, state: 'done' });
    channel.send({ step: submit, state: 'running', detail: 'job 4242 pending (Priority)' });
    channel.send({ step: submit, state: 'done', detail: 'Submitted batch job 4242' });
    desktop.daemons.set(NEW.id, fresh.daemon);
    await desktop.setWorkspaces([NEW]);
    channel.send({ step: connect, state: 'done' });
    channel.send({ step: 'add', state: 'done' });
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
    expect(screen.getByTestId('plan-steps').textContent).toContain('Copy pitcrewd 0.4.0 to ~/.pitcrew (plan-1)');
    // Verbatim: tabs, trailing spaces and blank lines included.
    expect(screen.getByTestId('job-script').textContent).toBe(SCRIPT);
    expect(screen.getByText('Nothing changes on the remote until you press Connect.')).toBeTruthy();
    expect(desktop.commands('gateway_remote_add')).toEqual([]);

    fireEvent.click(button('Connect'));
    await heading('Connecting hpc-login');
    // SSH asks; the shell's dialog answers.
    const dialog = await screen.findByRole('dialog', { name: 'hpc-login asks for a password' }, PATIENCE);
    // Meanwhile (behind the dialog, so hidden from the accessibility tree for now): every step of
    // the plan, each with its latest state and detail.
    const rows = within(screen.getByTestId('progress')).getAllByRole('listitem', { hidden: true });
    expect(rows.map((r) => r.textContent)).toEqual([
      'Copy pitcrewd 0.4.0 to ~/.pitcrew (plan-1)Running…40% sent',
      'Submit the job belowWaiting',
      'Connect and pairWaiting',
    ]);
    expect(screen.getByText(/A SLURM job may wait in the queue for several minutes/)).toBeTruthy();
    // And the live log: a line for each message, as it came.
    expect(screen.getByRole('log', { name: 'Install log', hidden: true }).textContent).toContain(
      'Copy pitcrewd 0.4.0 to ~/.pitcrew (plan-1): 40% sent',
    );
    fireEvent.change(within(dialog).getByLabelText('Password'), { target: { value: SECRET } });
    fireEvent.click(within(dialog).getByRole('button', { name: 'Send' }));

    await heading('Set up hpc-login');
    expect(within(screen.queryByTestId('progress') ?? document.body).queryByText('add')).toBeNull();
    expect(desktop.commands('gateway_remote_add').map((a) => a.plan)).toEqual(['plan-1']);
    expect(desktop.commands('gateway_prompt_reply')).toEqual([{ id: 'pw-1', answer: SECRET }]);

    // The fresh remote hub, through its own gateway transport.
    await screen.findByLabelText('Workspace name', undefined, PATIENCE);
    expect((screen.getByLabelText('Workspace name') as HTMLInputElement).value).toBe('hpc-login');
    expect((screen.getByLabelText('hpc-login’s name') as HTMLInputElement).value).toBe('hpc-login');
    fireEvent.change(screen.getByLabelText('Workspace name'), { target: { value: 'Cluster Lab' } });
    fireEvent.change(screen.getByLabelText('Your name'), { target: { value: 'Sam Rivera' } });
    fireEvent.change(screen.getByLabelText('hpc-login’s name'), { target: { value: 'Cluster job node' } });
    fireEvent.click(button('Set up'));

    // Signing in to the remote machine's agents, through its hub: skipped here. With SLURM the
    // logins run where the job runs, a compute node, which the hub's own machine names; and
    // compute nodes often have no internet.
    await heading('Sign in to your agents on hpc-login');
    await screen.findByText(/in a terminal on Cluster job node\./, undefined, PATIENCE);
    const note = screen.getByRole('note');
    expect(note.textContent).toContain('each login runs on its compute node, not on hpc-login itself');
    expect(note.textContent).toContain('no internet access');
    fireEvent.click(button('Skip for now'));
    await heading('Connected');
    expect(fresh.setups).toEqual([
      { workspace_name: 'Cluster Lab', person: { name: 'Sam Rivera', handle: '@sam' }, machine_name: 'Cluster job node' },
    ]);
    const setupRequest = desktop
      .commands('gateway_request')
      .map((a) => a.req as { workspace: string; method: string; path: string })
      .find((r) => r.method === 'POST');
    expect(setupRequest).toMatchObject({ workspace: NEW.id, path: '/v1/setup' });

    fireEvent.click(button('Open hpc-login'));
    await heading('Home');
    expect(router.state.location.pathname).toBe(`/w/${NEW.id}/home`);
    // Under StrictMode, each remote call that changes or asks something ran once.
    for (const cmd of ['gateway_remote_probe', 'gateway_remote_plan', 'gateway_remote_add', 'gateway_prompt_reply']) {
      expect(desktop.commands(cmd), cmd).toHaveLength(1);
    }
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
    expect(screen.getByTestId('plan-steps').textContent).toContain('(plan-1)');
    fireEvent.click(button('Connect'));

    await heading('Review: connect hpc-login');
    const notice = await screen.findByText(/The gateway refused that plan \(The plan has expired\.\)/);
    expect(notice.getAttribute('role')).toBe('status');
    // A fresh plan, shown before it can be submitted.
    await vi.waitFor(() => expect(screen.getByTestId('plan-steps').textContent).toContain('(plan-2)'));
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
    fireEvent.click(within(dialog).getByRole('button', { name: 'Cancel sign-in' }));

    const alert = await screen.findByRole('alert', undefined, PATIENCE);
    expect(alert.textContent).toBe('Connecting failed at “Copy pitcrewd 0.4.0 to ~/.pitcrew (plan-1)”: Authentication cancelled.');
    expect(desktop.commands('gateway_prompt_reply')).toEqual([{ id: 'pw-1' }]);
    expect(screen.queryByRole('dialog')).toBeNull();
    expect(screen.queryByText(/^Working\./)).toBeNull();
    expect(within(screen.getByTestId('progress')).getByText('Failed')).toBeTruthy();

    // The plan was used: back to Review means a fresh one.
    fireEvent.click(button('Back to review'));
    await heading('Review: connect hpc-login');
    await vi.waitFor(() => expect(screen.getByTestId('plan-steps').textContent).toContain('(plan-2)'));
  });

  it('can be left running: back to PitCrew, while the add goes on and its workspace shows up', async () => {
    let finish: () => Promise<void> = async () => {};
    desktop.add = (plan, channel) =>
      new Promise((resolve) => {
        const [copy = ''] = stepsOf(plan);
        channel.send({ step: copy, state: 'running' });
        finish = async () => {
          desktop.daemons.set(NEW.id, fresh.daemon);
          await desktop.setWorkspaces([NEW]);
          channel.send({ step: 'add', state: 'done' });
          resolve(NEW);
        };
      });
    const router = renderApp();
    await toReview();
    fireEvent.click(button('Connect'));
    await heading('Connecting hpc-login');
    fireEvent.click(button('Leave it running'));
    await screen.findByText('No workspaces yet.', undefined, PATIENCE);
    expect(router.state.location.pathname).toBe('/');
    expect(desktop.commands('gateway_remote_cancel')).toEqual([]);

    // The add finishes in the gateway; its workspace is listed, and `/` opens it (to its setup).
    await finish();
    await vi.waitFor(() => expect(router.state.location.pathname).toBe(`/w/${NEW.id}/onboarding`), PATIENCE);
  });

  it('stops a running add after asking, through gateway_remote_cancel', async () => {
    let fail: () => void = () => {};
    desktop.add = (plan, channel) =>
      new Promise((_resolve, reject) => {
        const [copy = '', submit = ''] = stepsOf(plan);
        channel.send({ step: copy, state: 'done' });
        channel.send({ step: submit, state: 'running', detail: 'job 4242 pending (Priority)' });
        fail = () => {
          channel.send({ step: submit, state: 'failed', detail: 'Job 4242 cancelled.' });
          channel.send({ step: 'add', state: 'failed', detail: 'Stopped at your request.' });
          reject({ code: 'unreachable', message: 'cancelled' });
        };
      });
    desktop.cancel = () => {
      fail();
      return Promise.resolve(null);
    };
    renderApp();
    await toReview();
    fireEvent.click(button('Connect'));
    await heading('Connecting hpc-login');
    fireEvent.click(button('Stop connecting…'));
    // It asks first: keeping on sends nothing.
    let confirm = await screen.findByRole('dialog', { name: 'Stop connecting hpc-login?' });
    fireEvent.click(within(confirm).getByRole('button', { name: 'Keep connecting' }));
    await vi.waitFor(() => expect(screen.queryByRole('dialog')).toBeNull());
    expect(desktop.commands('gateway_remote_cancel')).toEqual([]);

    fireEvent.click(button('Stop connecting…'));
    confirm = await screen.findByRole('dialog', { name: 'Stop connecting hpc-login?' });
    fireEvent.click(within(confirm).getByRole('button', { name: 'Stop connecting' }));
    const alert = await screen.findByRole('alert', undefined, PATIENCE);
    expect(alert.textContent).toBe('Connecting was stopped: Job 4242 cancelled.');
    expect(desktop.commands('gateway_remote_cancel')).toEqual([{ plan: 'plan-1' }]);
    expect(button('Back to review')).toBeTruthy();
  });

  it('says so when the gateway cannot stop an add yet, and keeps waiting', async () => {
    desktop.add = (plan, channel) => {
      channel.send({ step: stepsOf(plan)[0] ?? '', state: 'running' });
      return new Promise(() => {});
    };
    // FakeDesktop's default: "command gateway_remote_cancel not found", as an older gateway says.
    renderApp();
    await toReview();
    fireEvent.click(button('Connect'));
    await heading('Connecting hpc-login');
    fireEvent.click(button('Stop connecting…'));
    const confirm = await screen.findByRole('dialog', { name: 'Stop connecting hpc-login?' });
    fireEvent.click(within(confirm).getByRole('button', { name: 'Stop connecting' }));
    expect((await screen.findByRole('alert', undefined, PATIENCE)).textContent).toBe(
      'This version of the desktop app cannot stop a connection yet. Leave it running, then remove the workspace once it shows up.',
    );
    expect(button('Leave it running')).toBeTruthy();
    expect((button('Stop connecting…') as HTMLButtonElement).disabled).toBe(false);
  });

  it('shows a launch refused once under way as a failure, not as a new plan', async () => {
    desktop.add = (plan, channel) => {
      const [copy = '', submit = ''] = stepsOf(plan);
      channel.send({ step: copy, state: 'done' });
      channel.send({ step: submit, state: 'failed', detail: 'sbatch: error: invalid partition specified: gpu' });
      return refuse('invalid', 'The launch was refused.');
    };
    renderApp();
    await toReview();
    fireEvent.click(button('Connect'));
    const alert = await screen.findByRole('alert', undefined, PATIENCE);
    expect(alert.textContent).toBe('Connecting failed at “Submit the job below”: sbatch: error: invalid partition specified: gpu');
    expect(desktop.commands('gateway_remote_plan')).toHaveLength(1);
    fireEvent.click(button('Back to review'));
    await heading('Review: connect hpc-login');
    await vi.waitFor(() => expect(screen.getByTestId('plan-steps').textContent).toContain('(plan-2)'));
  });

  it('warns when the job script holds invisible or direction-changing characters', async () => {
    const tricky = `#!/bin/bash\nexec pitcrewd serve # ${String.fromCodePoint(0x202e)}lmao\r\n`;
    desktop.plan = () => ({ plan: 'plan-1', steps: stepsOf('plan-1'), jobScript: tricky });
    renderApp();
    await toReview();
    expect(screen.getByTestId('job-script').textContent).toBe(tricky);
    expect((await screen.findByRole('alert')).textContent).toContain('direction-changing characters, carriage returns');
  });

  it('can leave the new hub to set up later', async () => {
    desktop.add = addAsking();
    const router = renderApp();
    await toReview();
    fireEvent.click(button('Connect'));
    const dialog = await screen.findByRole('dialog', { name: 'hpc-login asks for a password' }, PATIENCE);
    fireEvent.change(within(dialog).getByLabelText('Password'), { target: { value: SECRET } });
    fireEvent.click(within(dialog).getByRole('button', { name: 'Send' }));
    await heading('Set up hpc-login');
    fireEvent.click(button('Set it up later'));
    await vi.waitFor(() => expect(router.state.location.pathname).not.toBe('/connect'));
    expect(fresh.setups).toEqual([]);
  });

  it("falls back on the whole add's detail when no step says why", async () => {
    desktop.add = async (plan, channel) => {
      const [copy = ''] = stepsOf(plan);
      channel.send({ step: copy, state: 'done' });
      channel.send({ step: 'add', state: 'failed', detail: 'The helper did not start: no space left on device.' });
      return refuse('unreachable', 'add failed');
    };
    renderApp();
    await toReview();
    fireEvent.click(button('Connect'));
    expect((await screen.findByRole('alert', undefined, PATIENCE)).textContent).toBe(
      'Connecting failed: The helper did not start: no space left on device.',
    );
  });

  it('offers tmux only from 3.2, and says when it is not known', async () => {
    desktop.probe = (host) => ({ host, os: 'linux', arch: 'x86_64', tmux: { version: '3.1c' } });
    renderApp();
    await heading('Connect a remote machine');
    fireEvent.click(await screen.findByRole('radio', { name: 'hpc-login' }));
    fireEvent.click(button('Continue'));
    await heading('Checking hpc-login');
    expect((await screen.findByTestId('probe')).textContent).toContain('tmux3.1c');
    fireEvent.click(button('Continue'));
    await heading('How PitCrew runs on hpc-login');
    const tmux = screen.getByRole('radio', { name: /In tmux/ }) as HTMLInputElement;
    expect(tmux.disabled).toBe(true);
    expect(screen.getByText('tmux 3.1c is too old here: PitCrew needs 3.2 or newer.')).toBeTruthy();
    // Not tmux, then: the launcher starts on one that can run.
    expect((screen.getByRole('radio', { name: /Directly/ }) as HTMLInputElement).checked).toBe(true);

    // Unknown: offered, with a note.
    fireEvent.click(button('Back'));
    desktop.probe = (host) => ({ host, os: 'linux', arch: 'x86_64' });
    await heading('Checking hpc-login');
    fireEvent.click(button('Back'));
    await heading('Connect a remote machine');
    fireEvent.click(button('Continue'));
    await heading('Checking hpc-login');
    expect((await screen.findByTestId('probe')).textContent).toContain('tmuxNot known');
    fireEvent.click(button('Continue'));
    await heading('How PitCrew runs on hpc-login');
    expect((screen.getByRole('radio', { name: /In tmux/ }) as HTMLInputElement).disabled).toBe(false);
    expect(screen.getByText('Whether hpc-login has tmux 3.2 or newer is not known: the plan says so if not.')).toBeTruthy();
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
    expect((await screen.findByRole('alert')).textContent).toBe(
      'A host is a name from your ssh config, or user@host: letters, digits, ".", "_" and "-" only.',
    );
    fireEvent.change(typed, { target: { value: 'sam@-oProxyCommand=sh' } });
    fireEvent.click(button('Continue'));
    expect((await screen.findByRole('alert')).textContent).toBe('A host cannot start with "-".');
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
