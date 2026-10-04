// @vitest-environment happy-dom
import { cleanup, fireEvent, render, screen, waitFor, within } from '@testing-library/react';
import { afterEach, beforeEach, expect, it, vi } from 'vitest';
import { DesktopUpdates, UpdateSettings } from '../updates.tsx';
const ipc = vi.hoisted(() => ({ invoke: vi.fn(), listen: vi.fn(), off: vi.fn() }));
vi.mock('@tauri-apps/api/core', () => ({ invoke: ipc.invoke }));
vi.mock('@tauri-apps/api/event', () => ({ listen: ipc.listen }));
const available = { enabled: true, prereleases: false, version: '1.2.3', notesUrl: 'https://example.com/releases' };
beforeEach(() => {
  ipc.invoke.mockReset();
  ipc.listen.mockReset();
  ipc.off.mockReset();
  ipc.listen.mockResolvedValue(ipc.off);
  ipc.invoke.mockResolvedValue(available);
});
afterEach(cleanup);
it('offers an update without installing and requires explicit confirmation', async () => {
  render(<DesktopUpdates><UpdateSettings /></DesktopUpdates>);
  await screen.findByText('PitCrew 1.2.3 is available.');
  expect(ipc.invoke.mock.calls.map((c) => c[0])).toEqual(['gateway_update_status']);
  fireEvent.click(screen.getByRole('button', { name: 'Update' }));
  await screen.findByRole('button', { name: 'Install and restart' });
  fireEvent.click(screen.getByRole('button', { name: 'Cancel' }));
  expect(ipc.invoke.mock.calls.map((c) => c[0])).toEqual(['gateway_update_status']);
  fireEvent.click(screen.getByRole('button', { name: 'Update' }));
  ipc.invoke.mockRejectedValueOnce('Signature verification failed');
  fireEvent.click(await screen.findByRole('button', { name: 'Install and restart' }));
  await waitFor(() => expect(ipc.invoke).toHaveBeenCalledWith('gateway_update_install', { version: '1.2.3' }));
  expect(within(screen.getByRole('dialog')).getByText('Signature verification failed')).toBeTruthy();
});
it('saves pre-release opt-in and checks now without installing', async () => {
  render(<DesktopUpdates><UpdateSettings /></DesktopUpdates>);
  const checkbox = await screen.findByRole('checkbox', { name: 'Include pre-releases' });
  await waitFor(() => expect((checkbox as HTMLInputElement).disabled).toBe(false));
  fireEvent.click(checkbox);
  await waitFor(() => expect(ipc.invoke).toHaveBeenCalledWith('gateway_update_channel', { prereleases: true }));
  await waitFor(() => expect((screen.getByRole('button', { name: 'Check now' }) as HTMLButtonElement).disabled).toBe(false));
  fireEvent.click(screen.getByRole('button', { name: 'Check now' }));
  await waitFor(() => expect(ipc.invoke).toHaveBeenCalledWith('gateway_update_check', undefined));
  expect(ipc.invoke.mock.calls.some((c) => c[0] === 'gateway_update_install')).toBe(false);
});
it('disables checking in unsigned builds and unsubscribes on unmount', async () => {
  ipc.invoke.mockResolvedValue({ enabled: false, prereleases: false });
  const view = render(<DesktopUpdates><UpdateSettings /></DesktopUpdates>);
  await screen.findByText(/Automatic updates are disabled in this build/);
  expect((screen.getByRole('button', { name: 'Check now' }) as HTMLButtonElement).disabled).toBe(true);
  view.unmount();
  expect(ipc.off).toHaveBeenCalledOnce();
});
