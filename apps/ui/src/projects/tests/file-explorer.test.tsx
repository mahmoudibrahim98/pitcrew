// @vitest-environment happy-dom
import { fireEvent, screen, waitFor, within } from '@testing-library/react';
import { afterEach, beforeEach, expect, it, vi } from 'vitest';
import { FileExplorer } from '../file-explorer.tsx';
import { FileBreadcrumbs } from '../file-breadcrumbs.tsx';
import { demo, otherClient, renderWithHub, startHub, stopHub, type Hub } from './harness.tsx';

let hub: Hub;
beforeEach(async () => { hub = await startHub(); });
afterEach(async () => { vi.restoreAllMocks(); await stopHub(hub); });
it('shows a collapsible explorer, hides dot and ignored names, and quick-opens a file in the right location', async () => {
  const stream = await otherClient(hub).workstream(demo.submission);
  const open = vi.fn();
  renderWithHub(<FileExplorer workstream={stream} openFile={open} />, hub);
  await screen.findByRole('button', { name: '▸ src' });
  expect(screen.queryByRole('button', { name: '.git' })).toBeNull();
  expect(screen.queryByRole('button', { name: 'debug.log' })).toBeNull();
  fireEvent.click(screen.getByLabelText('Show hidden'));
  await screen.findByRole('button', { name: '.git' });
  await screen.findByRole('button', { name: 'debug.log' });
  fireEvent.click(screen.getByLabelText('Show hidden'));
  fireEvent.keyDown(window, { key: 'p', ctrlKey: true });
  const dialog = await screen.findByRole('dialog', { name: 'Quick open' });
  fireEvent.change(within(dialog).getByLabelText('Filename'), { target: { value: 'shello' } });
  fireEvent.click(await within(dialog).findByRole('button', { name: 'src/hello.txt' }));
  expect(open).toHaveBeenCalledWith({ kind: 'file', workstream: stream.id, location: 0, path: 'src/hello.txt' });
  await waitFor(() => expect(screen.queryByRole('dialog')).toBeNull());
});
it('breadcrumbs navigate folders and copy the full path without rendering markup', async () => {
  const writeText = vi.fn().mockResolvedValue(undefined);
  Object.defineProperty(navigator, 'clipboard', { configurable: true, value: { writeText } });
  const folder = vi.fn();
  renderWithHub(<FileBreadcrumbs path="src/method.tex" copyPath="/home/sam/paper/src/method.tex" onFolder={folder} />, hub);
  fireEvent.click(screen.getByRole('button', { name: 'src' }));
  expect(folder).toHaveBeenCalledWith('src');
  fireEvent.click(screen.getByRole('button', { name: 'Copy path' }));
  await screen.findByText('Path copied');
  expect(writeText).toHaveBeenCalledWith('/home/sam/paper/src/method.tex');
});
