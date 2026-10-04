// @vitest-environment happy-dom
// @vitest-environment-options {"url": "http://localhost:5173/"}
// Outward writes in the UI, against a real mock hub (its writes answer from recorded fixtures,
// never the network): an approval in the Inbox shows exactly what will be sent and is answered in
// place; the task drawer shows pending and done writes, asks for new ones, and retries a failed
// one. Nothing is sent until "Send".
import { fireEvent, screen, waitFor, within } from '@testing-library/react';
import axe from 'axe-core';
import { afterEach, beforeEach, describe, expect, it } from 'vitest';
import type { Api, Task, UpstreamWrite } from '../../data/index.ts';
import { integrationClient } from '../integrations/api.ts';
import { Inbox } from '../inbox.tsx';
import { TaskDrawer } from '../task-drawer.tsx';
import { fieldRows, writeClient } from '../writes/api.ts';
import { demo, eventually, otherClient, renderWithHub, startHub, stopHub, type Hub } from './harness.tsx';

let hub: Hub;
beforeEach(async () => {
  hub = await startHub();
});
afterEach(async () => {
  await stopHub(hub);
});

/** Connects GitHub, links Seed runs to milestone 1, and syncs: issue #1 becomes a task. */
async function connected(api: Api): Promise<Task> {
  const added = await integrationClient(api).add({
    name: 'Demo repository',
    settings: { kind: 'github', repos: ['example-org/demo-repo'] },
    credential: 'gh_cli',
  });
  await integrationClient(api).link(demo.seedRuns, [{ system: 'github', key: 'example-org/demo-repo#milestone:1' }]);
  await integrationClient(api).sync(added.id);
  const tasks = await api.request<Task[]>('GET', '/v1/tasks', { query: { workstream: demo.seedRuns } });
  const task = tasks.find((t) => t.source?.key === 'example-org/demo-repo#1');
  if (task === undefined) throw new Error('issue #1 did not become a task');
  return task;
}

const move = (api: Api, task: Task, to: string) =>
  api.request<Task>('POST', `/v1/tasks/${task.id}/move`, { body: { to } });

describe('Outward writes', () => {
  it('an approval in the Inbox shows what will be sent, and Send sends it', async () => {
    const api = otherClient(hub);
    const task = await connected(api);
    await move(api, task, 'done');
    renderWithHub(<Inbox />, hub);
    const card = await screen.findByRole('region', { name: /Close example-org\/demo-repo#1 on GitHub/ });
    expect(within(card).getByText('Waiting for approval')).toBeTruthy();
    expect(within(card).getByText(/Nothing is sent to GitHub unless you choose Send/)).toBeTruthy();
    const row = card.querySelector('tr[data-field="state"]');
    expect(row?.textContent).toContain('open');
    expect(row?.textContent).toContain('closed (completed)');
    const results = await axe.run(card);
    expect(results.violations).toEqual([]);

    const [pending] = await writeClient(api).ofTask(task.id);
    expect(pending?.state).toBe('pending');
    fireEvent.click(within(card).getByRole('button', { name: 'Send' }));
    await eventually(async () => {
      const [write] = await writeClient(api).ofTask(task.id);
      expect(write?.state).toBe('sent');
    });
    // Answered: the approval leaves the Inbox.
    await waitFor(() => expect(screen.queryByRole('region', { name: /Close example-org/ })).toBeNull());
  });

  it('Don’t send records that nothing was sent', async () => {
    const api = otherClient(hub);
    const task = await connected(api);
    await move(api, task, 'canceled');
    renderWithHub(<Inbox />, hub);
    const card = await screen.findByRole('region', { name: /Close example-org\/demo-repo#1 on GitHub/ });
    expect(card.querySelector('tr[data-field="state"]')?.textContent).toContain('closed (not planned)');
    fireEvent.click(within(card).getByRole('button', { name: "Don't send" }));
    await eventually(async () => {
      const [write] = await writeClient(api).ofTask(task.id);
      expect(write?.state).toBe('not_sent');
      expect(write?.result).toMatchObject({ outcome: 'not_sent' });
    });
  });

  it('the drawer shows a task’s writes, asks for a comment and retries a failure', async () => {
    const api = otherClient(hub);
    const created = await api.request<Task>('POST', '/v1/tasks', {
      body: { project: demo.paper, workstream: demo.seedRuns, title: 'Synthetic new issue' },
    });
    const task = await connected(api);
    expect(task.id).not.toBe(created.id);
    renderWithHub(<TaskDrawer taskId={created.id} open onOpenChange={() => {}} />, hub);
    // A task that mirrors nothing can ask to create an issue; the ask waits in the Inbox.
    fireEvent.click(await screen.findByRole('button', { name: 'Create an issue upstream' }));
    await screen.findByText('Waiting for approval in the Inbox.');
    const list = await screen.findByRole('list', { name: 'Writes upstream' });
    expect(within(list).getByText(/Create an issue in example-org\/demo-repo on GitHub/)).toBeTruthy();
    expect(within(list).getByText('Waiting for approval')).toBeTruthy();
    const [create] = await writeClient(api).ofTask(created.id);
    if (create === undefined) throw new Error('no write');
    await api.request('POST', `/v1/asks/${create.proposal.ask}/answer`, { body: { option: 0 } });
    // Sent: the task mirrors the new issue, and can comment on it.
    await within(list).findByText('Created example-org/demo-repo#8', { exact: false });
    const comment = await screen.findByLabelText(/Comment on example-org\/demo-repo#8/);
    fireEvent.change(comment, { target: { value: 'Synthetic comment.' } });
    fireEvent.click(screen.getByRole('button', { name: 'Ask to comment' }));
    let commentWrite: UpstreamWrite | undefined;
    await eventually(async () => {
      commentWrite = (await writeClient(api).ofTask(created.id)).find((w) => w.proposal.operation === 'comment');
      expect(commentWrite?.state).toBe('pending');
    });
    // The fixture refuses comments on #8: failed, with upstream's message; Retry sends it again.
    await api.request('POST', `/v1/asks/${commentWrite?.proposal.ask}/answer`, { body: { option: 0 } });
    const failed = await within(list).findByText('Failed');
    const item = failed.closest('li');
    if (item === null) throw new Error('no item');
    expect(within(item).getByText(/Validation Failed/)).toBeTruthy();
    fireEvent.click(within(item).getByRole('button', { name: 'Retry' }));
    await within(item).findByText('2 attempts');
    // What it sends, on demand.
    fireEvent.click(within(item).getByRole('button', { name: 'What it sends' }));
    expect(item.querySelector('tr[data-field="comment"]')?.textContent).toContain('Synthetic comment.');
  });

  it('diff rows name every field sent, with upstream’s value before', () => {
    const write: UpstreamWrite = {
      proposal: {
        ask: '01J00000000000000000000000',
        integration: '01J00000000000000000000000',
        system: 'jira',
        scope: 'DEMO',
        target: { system: 'jira', key: 'DEMO-6' },
        operation: 'update',
        before: { title: 'Old', labels: ['billing'] },
        after: { title: 'New', labels: [], epic: 'DEMO-5' },
        requested_by: demo.sam,
      },
      state: 'pending',
      attempts: 0,
      proposed_at: 0,
    };
    expect(fieldRows(write)).toEqual([
      { field: 'summary', before: 'Old', after: 'New' },
      { field: 'labels', before: 'billing', after: '(none)' },
      { field: 'epic', after: 'DEMO-5' },
    ]);
  });
});
