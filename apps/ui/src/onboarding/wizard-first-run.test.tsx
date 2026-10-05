// @vitest-environment happy-dom
//
// Walks the first-run wizard end to end against the fake `OnboardingApi` (which has every step):
// every step renders, validates where it should, and advances; the skippable steps can be skipped
// instead.

import { cleanup, fireEvent, screen, within } from '@testing-library/react';
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

function rowOf(element: HTMLElement): HTMLElement {
  const row = element.closest('li');
  if (row === null) throw new Error('No <li> ancestor found');
  return row;
}

/** The `<li>` row containing an element found by its text. */
async function rowForTextAsync(text: string | RegExp): Promise<HTMLElement> {
  return rowOf(await screen.findByText(text));
}

/** Fills the workspace step: a name, a person (the handle follows), and keeps the machine's name. */
function fillWorkspace(name = 'My team') {
  fireEvent.change(screen.getByLabelText('Workspace name'), { target: { value: name } });
  fireEvent.change(screen.getByLabelText('Your name'), { target: { value: 'Sam Rivera' } });
}

describe('the first-run wizard', () => {
  it('runs start to finish, validating, fixing, streaming and skipping along the way', async () => {
    renderWizard(createFakeOnboardingApi({ speed: 0 }));

    // 1. Welcome: theme and density, no validation, always advances.
    await heading('Welcome to PitCrew');
    fireEvent.click(screen.getByRole('radio', { name: 'Dark' }));
    fireEvent.click(screen.getByRole('button', { name: 'Get started' }));

    // 2. Workspace: empty names are refused by their fields; the handle follows the name.
    await heading('Your first workspace');
    fireEvent.click(continueButton());
    const alerts = await screen.findAllByRole('alert');
    expect(alerts.map((a) => a.textContent)).toEqual(['Give the workspace a name.', 'Enter your name.', 'Give yourself a handle.']);
    expect(screen.getByLabelText('Workspace name')).toBe(document.activeElement);
    fillWorkspace();
    expect((screen.getByLabelText('Your handle') as HTMLInputElement).value).toBe('@sam');
    expect((screen.getByLabelText('This machine’s name') as HTMLInputElement).value).toBe('This computer');
    fireEvent.click(continueButton());

    // 3. Machine check: waits for the check, then a missing tool's fix (the fake pretends it was
    // installed from its page).
    await heading('Checking the machine');
    const opencodeRow = await rowForTextAsync('OpenCode CLI');
    expect(within(opencodeRow).getByText('Missing')).toBeTruthy();
    fireEvent.click(within(opencodeRow).getByRole('button', { name: 'Install OpenCode CLI…' }));
    await within(opencodeRow).findByText('OK');
    fireEvent.click(continueButton());

    // 4. Install helper: the direct launcher on this computer shows no script preview.
    await heading('Install the helper');
    fireEvent.click(await screen.findByRole('button', { name: 'Install the helper' }));
    expect(screen.queryByText(/exact script/)).toBeNull();
    await screen.findByText('Helper installed.');
    fireEvent.click(continueButton());

    // 5. Sign in: skippable.
    await heading('Sign in to your agents');
    await screen.findByText('Claude Code');
    fireEvent.click(screen.getByRole('button', { name: 'Skip for now' }));

    // 6. Integrations: skippable.
    await heading('Connect integrations');
    fireEvent.click(screen.getByRole('button', { name: 'Skip integrations' }));

    // 7. Scan: streams, then shows counts; Continue is disabled until it is done.
    await heading('Scanning for sessions');
    await screen.findByText(/likely project/);
    expect(continueButton().disabled).toBe(false);
    fireEvent.click(continueButton());

    // 8. Create: untick one suggestion, rename another, then create.
    await heading('Create projects and workstreams');
    const scratchCheckbox = await screen.findByRole('checkbox', { name: 'Include Scratch' });
    fireEvent.click(scratchCheckbox);
    const diffusionRow = rowOf(screen.getByRole('checkbox', { name: 'Include Diffusion study' }));
    fireEvent.change(within(diffusionRow).getByLabelText('Project name'), {
      target: { value: 'Diffusion study (renamed)' },
    });
    fireEvent.click(screen.getByRole('button', { name: 'Create' }));

    // 9. Import: the dry-run count updates with the filter, then "start fresh" imports nothing.
    await heading('Import sessions');
    await screen.findByText('This will import 56 sessions.');
    fireEvent.click(screen.getByRole('radio', { name: /Start fresh/ }));
    await screen.findByText('This will import 0 sessions.');
    fireEvent.click(continueButton());

    // 10. Hooks: shows the diff, then installs.
    await heading('Install hooks');
    await screen.findByText('~/.claude/settings.json');
    fireEvent.click(screen.getByRole('button', { name: 'Install hooks' }));

    // 11. Safety: the hourly cap keeps its place, and only enables after opting in.
    await heading('Safety settings');
    expect(await screen.findByLabelText('Up to')).toHaveProperty('disabled', true);
    fireEvent.click(screen.getByRole('checkbox', { name: /low-risk agent requests/i }));
    expect(await screen.findByLabelText('Up to')).toHaveProperty('disabled', false);
    fireEvent.click(continueButton());

    // 12. Done: summarises the run and links Home.
    await heading("You're set up");
    const summary = await screen.findByText(/is ready, with you as/);
    expect(summary.textContent).toBe('“My team” is ready, with you as Sam Rivera (@sam) on This computer.');
    fireEvent.click(screen.getByRole('button', { name: 'Go to Home' }));
    await screen.findByRole('heading', { level: 1, name: 'Home' });
  }, 20_000);

  it('never sends setup twice: Back to the workspace step only shows what was set', async () => {
    renderWizard(createFakeOnboardingApi({ speed: 0 }));
    await heading('Welcome to PitCrew');
    fireEvent.click(screen.getByRole('button', { name: 'Get started' }));
    await heading('Your first workspace');
    fillWorkspace();
    fireEvent.click(continueButton());
    await heading('Checking the machine');
    fireEvent.click(screen.getByRole('button', { name: 'Back' }));
    await heading('Your first workspace');
    expect(screen.queryByLabelText('Workspace name')).toBeNull();
    expect(screen.getByText('“My team” is set up, with you as Sam Rivera (@sam).')).toBeTruthy();
    fireEvent.click(continueButton());
    await heading('Checking the machine');
  });

  it('keeps an explicitly chosen launcher on Back and Forward (review r1, item 1)', async () => {
    renderWizard(createFakeOnboardingApi({ speed: 0 }));
    await heading('Welcome to PitCrew');
    fireEvent.click(screen.getByRole('button', { name: 'Get started' }));

    await heading('Your first workspace');
    fillWorkspace();
    fireEvent.click(continueButton());

    await heading('Checking the machine');
    await rowForTextAsync('OpenCode CLI');
    fireEvent.click(continueButton());

    // 'direct' is the recommended (and so default) launcher for "This computer" in the fake; pick
    // the non-recommended 'tmux' instead.
    await heading('Install the helper');
    await screen.findByRole('radio', { name: /^direct/i, checked: true });
    fireEvent.click(screen.getByRole('radio', { name: /^tmux$/ }));
    await screen.findByRole('radio', { name: /^tmux$/, checked: true });

    // Back to machine check, then Forward again: the step unmounts and remounts, so a naive
    // "refetch options and reset to the recommended default" would silently discard the choice.
    fireEvent.click(screen.getByRole('button', { name: 'Back' }));
    await heading('Checking the machine');
    fireEvent.click(continueButton());

    await heading('Install the helper');
    await screen.findByRole('radio', { name: /^tmux$/, checked: true });
    expect(screen.getByRole('radio', { name: /^direct/i, checked: false })).toBeTruthy();
  });

  it('the stepper only lets you jump to a step already reached', async () => {
    renderWizard(createFakeOnboardingApi({ speed: 0 }));
    await heading('Welcome to PitCrew');
    const machineCheckTab = screen.getByRole('tab', { name: /Machine check/ }) as HTMLButtonElement;
    expect(machineCheckTab.disabled).toBe(true);
    fireEvent.click(screen.getByRole('button', { name: 'Get started' }));
    await heading('Your first workspace');
    // Radix Tabs activates on focus (the WAI-ARIA tabs pattern's "automatic activation"), which a
    // real click also gives the clicked button; `fireEvent.click` alone does not move focus.
    const welcomeTab = screen.getByRole('tab', { name: /Welcome/ });
    fireEvent.focus(welcomeTab);
    await heading('Welcome to PitCrew');
  });
});
