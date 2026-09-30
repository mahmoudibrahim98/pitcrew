// @vitest-environment happy-dom
// @vitest-environment-options {"url": "http://localhost:5173/"}

import { cleanup, fireEvent, screen, within } from '@testing-library/react';
import { afterEach, describe, expect, it, vi } from 'vitest';
import type { Task, Workstream } from '../../data/index.ts';
import { SessionHeader } from '../session-header.tsx';
import { ID, renderWithHub, startHub, type HubProcess } from './harness.tsx';

describe('SessionHeader against the mock hub', () => {
  let hub: HubProcess | undefined;

  afterEach(async () => {
    cleanup();
    await hub?.close();
    hub = undefined;
  });

  it('shows the session and links to its task and workstream', async () => {
    hub = await startHub();
    const onOpenTask = vi.fn<(task: Task) => void>();
    const onOpenWorkstream = vi.fn<(workstream: Workstream) => void>();
    renderWithHub(
      hub,
      <SessionHeader sessionId={ID.ses1} onOpenTask={onOpenTask} onOpenWorkstream={onOpenWorkstream} />,
    );
    await screen.findByRole('heading', { name: 'Draft method section' });
    const header = screen.getByRole('banner');
    await vi.waitFor(() => expect(header.textContent).toContain('This laptop'));
    for (const text of ['Working', 'Claude', '@writer', 'main', '/home/sam/work/diffusion-paper/paper']) {
      expect(header.textContent).toContain(text);
    }
    const work = within(header).getByRole('navigation', { name: 'Linked work' });
    fireEvent.click(await within(work).findByRole('button', { name: 'PAP-1 · Draft the method section' }));
    expect(onOpenTask).toHaveBeenCalledWith(expect.objectContaining({ key: 'PAP-1' }));
    fireEvent.click(within(work).getByRole('button', { name: 'Submission' }));
    expect(onOpenWorkstream).toHaveBeenCalledWith(expect.objectContaining({ name: 'Submission' }));
  });

  it('marks an unreachable machine and offers no End for it', async () => {
    hub = await startHub();
    renderWithHub(hub, <SessionHeader sessionId={ID.ses5} />);
    await screen.findByRole('heading', { name: 'Try a cosine schedule' });
    await vi.waitFor(() => expect(screen.getByRole('banner').textContent).toContain('gpu-box(unreachable)'));
    fireEvent.keyDown(screen.getByRole('button', { name: 'Session actions' }), { key: 'Enter' });
    const menu = await screen.findByRole('menu');
    expect(within(menu).getByRole('menuitem', { name: 'End session…' }).getAttribute('aria-disabled')).toBe('true');
  });

  it('ends the session from the actions menu after a confirmation', async () => {
    hub = await startHub();
    const { requests } = renderWithHub(hub, <SessionHeader sessionId={ID.ses4} />);
    await screen.findByRole('heading', { name: 'Codex rollout parser' });
    fireEvent.keyDown(screen.getByRole('button', { name: 'Session actions' }), { key: 'Enter' });
    const menu = await screen.findByRole('menu');
    for (const name of ['Hand off', 'Fork', 'Review']) {
      expect(within(menu).getByRole('menuitem', { name: new RegExp(`^${name}`) }).getAttribute('aria-disabled')).toBe(
        'true',
      );
    }
    fireEvent.click(within(menu).getByRole('menuitem', { name: 'End session…' }));
    const confirm = await screen.findByRole('alertdialog', { name: 'End this session?' });
    fireEvent.click(within(confirm).getByRole('button', { name: 'Kill now' }));
    await vi.waitFor(() => expect(screen.getByRole('banner').textContent).toContain('Ended'), { timeout: 4_000 });
    expect(requests.filter((r) => r.path === `/v1/sessions/${ID.ses4}/end`).map((r) => r.body)).toEqual([
      { mode: 'kill' },
    ]);
    expect(screen.queryByRole('alertdialog')).toBeNull();
  });
});
