// @vitest-environment happy-dom
import { cleanup, fireEvent, render, screen, waitFor } from '@testing-library/react';
import { afterEach, expect, it, vi } from 'vitest';
import { OnboardingApiProvider } from './api-context.tsx';
import { createFakeOnboardingApi } from './fake-api.ts';
import { WizardProvider } from './wizard-context.tsx';
import { HooksStep } from './steps/hooks-step.tsx';
import { SafetyStep } from './steps/safety-step.tsx';

afterEach(cleanup);

it('only confirms the displayed revision and refreshes after stale refusal', async () => {
  const preview = { revision: 'synthetic-first', engines: [], files: [{ path: '/home/sam/.codex/config.toml', before: 'before\r\n', after: 'after\r\n' }] };
  const second = { ...preview, revision: 'synthetic-second' };
  const hooksDiff = vi.fn().mockResolvedValueOnce(preview).mockResolvedValueOnce(second);
  const installHooks = vi.fn().mockRejectedValueOnce(new Error('Preview is stale.')).mockResolvedValue(undefined);
  const api = { ...createFakeOnboardingApi({ speed: 0 }), hooksDiff, installHooks };
  render(<OnboardingApiProvider api={api}><WizardProvider><HooksStep /></WizardProvider></OnboardingApiProvider>);
  await screen.findByText('/home/sam/.codex/config.toml');
  expect(installHooks).not.toHaveBeenCalled();
  fireEvent.click(screen.getByRole('button', { name: 'Install hooks' }));
  await screen.findByRole('alert');
  expect(installHooks).toHaveBeenLastCalledWith(preview);
  fireEvent.click(screen.getByRole('button', { name: 'Refresh diff' }));
  await waitFor(() => expect(screen.queryByRole('alert')).toBeNull());
  fireEvent.click(screen.getByRole('button', { name: 'Install hooks' }));
  await waitFor(() => expect(installHooks).toHaveBeenLastCalledWith(second));
});

it('loads saved safety, retries a read failure, warns and keeps a failed save editable', async () => {
  const settings = { permissionMode: 'plan' as const, backOfficeEnabled: true, backOfficeCaps: { maxAutoAcceptPerHour: 7 } };
  const readSafety = vi.fn().mockRejectedValueOnce(new Error('Read unavailable.')).mockResolvedValue(settings);
  const saveSafety = vi.fn().mockRejectedValueOnce(new Error('Save unavailable.'));
  const api = { ...createFakeOnboardingApi({ speed: 0 }), readSafety, saveSafety };
  render(<OnboardingApiProvider api={api}><WizardProvider><SafetyStep /></WizardProvider></OnboardingApiProvider>);
  await screen.findByText('Read unavailable.');
  expect((screen.getByRole('button', { name: 'Continue' }) as HTMLButtonElement).disabled).toBe(true);
  fireEvent.click(screen.getByRole('button', { name: 'Try again' }));
  const cap = await screen.findByLabelText('Up to');
  expect((cap as HTMLInputElement).value).toBe('7');
  expect((screen.getByRole('radio', { name: /Plan first/ }) as HTMLInputElement).checked).toBe(true);
  fireEvent.click(screen.getByRole('radio', { name: /Skip permissions/ }));
  await screen.findByText(/Warning: skipping permissions/);
  fireEvent.click(screen.getByRole('button', { name: 'Continue' }));
  await screen.findByText('Save unavailable.');
  expect(saveSafety).toHaveBeenCalledWith({ ...settings, permissionMode: 'bypass-permissions' });
  expect((screen.getByRole('button', { name: 'Continue' }) as HTMLButtonElement).disabled).toBe(false);
});
