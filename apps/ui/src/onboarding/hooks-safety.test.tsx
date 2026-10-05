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
  expect(screen.getByRole('radio', { name: /Skip permissions/ })).toHaveProperty('disabled', true);
  await screen.findByText(/runner currently disallows/);
  fireEvent.click(screen.getByRole('button', { name: 'Continue' }));
  await screen.findByText('Save unavailable.');
  expect(saveSafety).toHaveBeenCalledWith(settings);
  expect((screen.getByRole('button', { name: 'Continue' }) as HTMLButtonElement).disabled).toBe(false);
});

it('does not install an empty preview and allows nonconflicting files', async () => {
  const empty = { revision: 'empty', engines: [], files: [] };
  const installHooks = vi.fn();
  const api = { ...createFakeOnboardingApi({ speed: 0 }), hooksDiff: vi.fn().mockResolvedValue(empty), installHooks };
  render(<OnboardingApiProvider api={api}><WizardProvider><HooksStep /></WizardProvider></OnboardingApiProvider>);
  await screen.findByText('No supported agent CLIs were found on this hub.');
  expect(screen.getByRole('button', { name: 'Install hooks' })).toHaveProperty('disabled', true);
  expect(installHooks).not.toHaveBeenCalled();
  cleanup();
  api.hooksDiff.mockResolvedValue({ revision: 'mixed', engines: [{engine: 'codex', status: 'conflicting', detail: 'Foreign notify.'}], files: [{path: '/home/sam/.config/opencode/plugin/pitcrew.js', before: null, after: 'synthetic plugin'}] });
  render(<OnboardingApiProvider api={api}><WizardProvider><HooksStep /></WizardProvider></OnboardingApiProvider>);
  await screen.findByText('codex: Foreign notify.');
  expect(screen.getByRole('button', { name: 'Install hooks' })).toHaveProperty('disabled', false);
  fireEvent.click(screen.getByRole('button', { name: 'Install hooks' }));
  await waitFor(() => expect(installHooks).toHaveBeenCalledOnce());
});
