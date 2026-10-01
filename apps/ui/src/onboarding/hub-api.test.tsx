// @vitest-environment happy-dom
//
// The real `OnboardingApi` (`createHubOnboardingApi`): setup is `POST /v1/setup` (here a stand-in
// for the data layer's `setUp`), the host list is the gateway's `sshHosts`, and every other call
// is unavailable, so the first run is Welcome, Workspace, Done. The hub's refusals land by the
// right field, and a workspace set up meanwhile goes Home.

import { cleanup, fireEvent, screen } from '@testing-library/react';
import { afterEach, describe, expect, it, vi } from 'vitest';
import { ApiError, SetupConflict, type RemoteGateway, type Setup, type SetupResult } from '../data/index.ts';
import { SetupRefused } from './api.ts';
import { createHubOnboardingApi } from './hub-api.ts';
import { stepsFor } from './steps.ts';
import { renderWizard, TEST_WS } from './test-support.tsx';

afterEach(() => cleanup());

const RESULT: SetupResult = {
  workspace: { id: '01JB000000000000000WSPFRSH', name: 'Demo Lab' },
  me: { id: '01JB000000000000000MEM0001', kind: 'human', handle: '@sam', name: 'Sam Rivera' },
  machine: { id: '01JB000000000000000MAC0001', name: 'This laptop', kind: 'local', liveness: 'live' },
};

const INPUT = { workspaceName: 'Demo Lab', person: { name: 'Sam Rivera', handle: '@sam' }, machineName: 'This laptop' };

function remoteWith(hosts: string[]): RemoteGateway {
  const no = () => Promise.reject(new Error('not in this test'));
  return {
    sshHosts: () => Promise.resolve(hosts),
    remoteProbe: no,
    remotePlan: no,
    remoteAdd: no,
    workspaceRemove: no,
    workspaceRetry: no,
    onPrompt: no,
    onPromptClosed: no,
    replyPrompt: no,
  };
}

describe('createHubOnboardingApi', () => {
  it('serves setup and the host list, and marks everything else unavailable', async () => {
    const setUp = vi.fn<(setup: Setup) => Promise<SetupResult>>(() => Promise.resolve(RESULT));
    const api = createHubOnboardingApi({ setUp, remote: remoteWith(['hpc-login', 'build-box']) });
    expect([...api.unavailable]).not.toContain('setupWorkspace');
    expect([...api.unavailable]).not.toContain('discoverHosts');
    expect(stepsFor(api).map((s) => s.id)).toEqual(['welcome', 'workspace', 'done']);

    expect(await api.discoverHosts()).toEqual([
      { kind: 'ssh', id: 'hpc-login' },
      { kind: 'ssh', id: 'build-box' },
    ]);
    expect(await api.setupWorkspace(INPUT)).toEqual({
      workspace: { id: RESULT.workspace.id, name: 'Demo Lab' },
      me: { name: 'Sam Rivera', handle: '@sam' },
    });
    expect(setUp).toHaveBeenCalledWith({
      workspace_name: 'Demo Lab',
      person: { name: 'Sam Rivera', handle: '@sam' },
      machine_name: 'This laptop',
    });

    await expect(api.checkMachine({ kind: 'local' })).rejects.toThrow('not available yet');
    await expect(api.saveSafety({ permissionMode: 'default', backOfficeEnabled: false, backOfficeCaps: { maxAutoAcceptPerHour: 1 } })).rejects.toThrow();
  });

  it('has no setup outside a workspace, and no hosts in a browser', async () => {
    const api = createHubOnboardingApi({ remote: null });
    expect(api.unavailable.has('setupWorkspace')).toBe(true);
    expect(api.unavailable.has('discoverHosts')).toBe(true);
    await expect(api.discoverHosts()).rejects.toThrow('not available yet');
    await expect(api.setupWorkspace(INPUT)).rejects.toThrow('not available yet');
  });

  it.each([
    [new SetupConflict(new ApiError('conflict', 'This workspace is already set up.', 409), true), { alreadySetUp: true, field: undefined }],
    [new SetupConflict(new ApiError('conflict', 'The handle @sam is already taken.', 409), false), { alreadySetUp: false, field: 'handle' }],
    [new ApiError('invalid', 'machine_name must be 1 to 60 characters.', 400), { alreadySetUp: false, field: 'machineName' }],
    [new ApiError('invalid', 'The body is not JSON.', 400), { alreadySetUp: false, field: undefined }],
  ])('turns %s into a SetupRefused', async (error, expected) => {
    const api = createHubOnboardingApi({ setUp: () => Promise.reject(error) });
    const refused = await api.setupWorkspace(INPUT).catch((e: unknown) => e);
    expect(refused).toBeInstanceOf(SetupRefused);
    expect(refused).toMatchObject({ ...expected, message: error.message });
  });

  it('passes any other failure on as it is', async () => {
    const offline = new ApiError('unavailable', 'Cannot reach the hub', 0);
    const api = createHubOnboardingApi({ setUp: () => Promise.reject(offline) });
    expect(await api.setupWorkspace(INPUT).catch((e: unknown) => e)).toBe(offline);
  });
});

describe('the real first run', () => {
  function walkToWorkspace() {
    return screen.findByRole('heading', { level: 1, name: 'Welcome to PitCrew' }).then(async () => {
      fireEvent.click(screen.getByRole('button', { name: 'Get started' }));
      await screen.findByRole('heading', { level: 1, name: 'Your first workspace' });
      fireEvent.change(screen.getByLabelText('Workspace name'), { target: { value: '  Demo Lab ' } });
      fireEvent.change(screen.getByLabelText('Your name'), { target: { value: 'Sam Rivera' } });
      fireEvent.change(screen.getByLabelText('This machine’s name'), { target: { value: 'This laptop' } });
    });
  }

  it('is Welcome, Workspace, Done, sending the trimmed names once', async () => {
    const setUp = vi.fn<(setup: Setup) => Promise<SetupResult>>(() => Promise.resolve(RESULT));
    const { router } = renderWizard(createHubOnboardingApi({ setUp }));
    await walkToWorkspace();
    expect(screen.getAllByRole('tab').map((t) => t.textContent?.replace(/^\d/, ''))).toEqual(['Welcome', 'Workspace', 'Done']);
    fireEvent.click(screen.getByRole('button', { name: 'Continue' }));
    await screen.findByRole('heading', { level: 1, name: "You're set up" });
    expect(setUp).toHaveBeenCalledTimes(1);
    expect(setUp.mock.calls[0]?.[0]).toEqual({
      workspace_name: 'Demo Lab',
      person: { name: 'Sam Rivera', handle: '@sam' },
      machine_name: 'This laptop',
    });
    fireEvent.click(screen.getByRole('button', { name: 'Go to Home' }));
    await screen.findByRole('heading', { level: 1, name: 'Home' });
    expect(router.state.location.pathname).toBe(`/w/${TEST_WS}/home`);
  });

  it("shows the hub's 400 by the field it names", async () => {
    const setUp = vi.fn(() => Promise.reject(new ApiError('invalid', 'machine_name must not contain control characters.', 400)));
    renderWizard(createHubOnboardingApi({ setUp }));
    await walkToWorkspace();
    fireEvent.click(screen.getByRole('button', { name: 'Continue' }));
    const alert = await screen.findByRole('alert');
    expect(alert.textContent).toBe('machine_name must not contain control characters.');
    const machine = screen.getByLabelText('This machine’s name');
    expect(machine.getAttribute('aria-invalid')).toBe('true');
    expect(machine.getAttribute('aria-describedby')).toContain(alert.id);
    expect(document.activeElement).toBe(machine);
  });

  it('shows a taken handle by the handle', async () => {
    const taken = new SetupConflict(new ApiError('conflict', 'The handle @sam is already taken.', 409), false);
    renderWizard(createHubOnboardingApi({ setUp: () => Promise.reject(taken) }));
    await walkToWorkspace();
    fireEvent.click(screen.getByRole('button', { name: 'Continue' }));
    const alert = await screen.findByRole('alert');
    expect(alert.textContent).toBe('The handle @sam is already taken.');
    expect(screen.getByLabelText('Your handle').getAttribute('aria-invalid')).toBe('true');
    expect(screen.getByRole('heading', { level: 1 }).textContent).toBe('Your first workspace');
  });

  it('goes Home when the workspace was set up meanwhile', async () => {
    const done = new SetupConflict(new ApiError('conflict', 'This workspace is already set up.', 409), true);
    const { router } = renderWizard(createHubOnboardingApi({ setUp: () => Promise.reject(done) }));
    await walkToWorkspace();
    fireEvent.click(screen.getByRole('button', { name: 'Continue' }));
    await screen.findByRole('heading', { level: 1, name: 'Home' });
    expect(router.state.location.pathname).toBe(`/w/${TEST_WS}/home`);
  });

  it('checks the form before sending anything', async () => {
    const setUp = vi.fn<(setup: Setup) => Promise<SetupResult>>(() => Promise.resolve(RESULT));
    renderWizard(createHubOnboardingApi({ setUp }));
    await walkToWorkspace();
    fireEvent.change(screen.getByLabelText('Your handle'), { target: { value: '@Sam' } });
    fireEvent.click(screen.getByRole('button', { name: 'Continue' }));
    expect((await screen.findByRole('alert')).textContent).toBe('A handle is "@" and 1 to 32 lower-case letters, digits, "_" or "-".');
    // The person typed a handle: the name no longer changes it.
    fireEvent.change(screen.getByLabelText('Your name'), { target: { value: 'Alex Kim' } });
    expect((screen.getByLabelText('Your handle') as HTMLInputElement).value).toBe('@Sam');
    expect(setUp).not.toHaveBeenCalled();
  });
});
