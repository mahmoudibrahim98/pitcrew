// @vitest-environment happy-dom
//
// The add-a-machine wizard reuses the first-run steps in a shorter order (no welcome,
// integrations, hooks or safety) and picks an SSH host, exercising the SLURM script preview on a
// different path than the first-run test (which stays on "this computer").

import { cleanup, fireEvent, screen } from '@testing-library/react';
import { afterEach, describe, expect, it } from 'vitest';
import { createFakeOnboardingApi } from './fake-api.ts';
import { renderWizard } from './test-support.tsx';

afterEach(() => cleanup());

function heading(name: string | RegExp) {
  return screen.findByRole('heading', { level: 1, name });
}

function continueButton(): HTMLButtonElement {
  return screen.getByRole('button', { name: 'Continue' }) as HTMLButtonElement;
}

describe('the add-a-machine wizard', () => {
  it('skips the workspace-only steps and shows the SLURM preview for an SSH host', async () => {
    renderWizard('add-machine', createFakeOnboardingApi({ speed: 0 }));

    // Starts straight on the machine picker: no welcome step, no workspace name field.
    await heading('Add a machine');
    expect(screen.queryByLabelText('Workspace name')).toBeNull();
    const sshOption = await screen.findByRole('radio', { name: /hpc-login/ });
    fireEvent.click(sshOption);
    fireEvent.click(continueButton());

    await heading('Checking the machine');
    await screen.findByText('SLURM');
    fireEvent.click(continueButton());

    await heading('Install the helper');
    fireEvent.click(await screen.findByRole('radio', { name: /SLURM batch job/ }));
    fireEvent.click(screen.getByRole('button', { name: 'Install the helper' }));
    await screen.findByText(/The exact script PitCrew will submit/);
    await screen.findByText('Helper installed.');
    fireEvent.click(continueButton());

    // No integrations step between sign-in and scan in this flow.
    await heading('Sign in to your agents');
    fireEvent.click(screen.getByRole('button', { name: 'Skip for now' }));
    await heading('Scanning for sessions');
    await screen.findByText(/likely project/);
    fireEvent.click(continueButton());

    await heading('Create projects and workstreams');
    fireEvent.click(screen.getByRole('button', { name: "Don't create any yet" }));

    await heading('Import sessions');
    fireEvent.click(screen.getByRole('button', { name: 'Skip' }));

    // No hooks or safety step: straight to done, with add-machine copy.
    await heading('Machine added');
    const summary = await screen.findByText(/is connected/);
    expect(summary.textContent ?? '').toContain('hpc-login');
    expect(screen.getByRole('button', { name: 'Done' })).toBeTruthy();
  }, 20_000);
});
