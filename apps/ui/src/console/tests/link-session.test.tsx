// @vitest-environment happy-dom
// @vitest-environment-options {"url": "http://localhost:5173/"}
import { fireEvent, screen, within } from '@testing-library/react';
import { afterEach, expect, it } from 'vitest';
import { SessionHeader } from '../session-header.tsx';
import { eventually, ID, renderWithHub, startHub, unmountAndSettle, type HubProcess } from './harness.tsx';

let hub: HubProcess | undefined;
afterEach(async () => { await unmountAndSettle(); await hub?.close(); hub = undefined; });

it('offers only tasks in the chosen workstream, resets the task, and refreshes through the stream', async () => {
  hub = await startHub();
  const { requests } = renderWithHub(hub, <SessionHeader sessionId={ID.ses5} />);
  await screen.findByRole('heading', { name: 'Try a cosine schedule' });
  fireEvent.keyDown(screen.getByRole('button', { name: 'Session actions' }), { key: 'Enter' });
  fireEvent.click(await screen.findByRole('menuitem', { name: 'Link to…' }));
  const dialog = await screen.findByRole('dialog', { name: 'Link session' });
  const workstream = within(dialog).getByLabelText('Workstream');
  await eventually(() => expect((workstream as HTMLSelectElement).disabled).toBe(false));
  fireEvent.change(workstream, { target: { value: '01JB000000000000000WST0001' } });
  const task = within(dialog).getByLabelText('Task (optional)');
  expect(task.textContent).toContain('PAP-1');
  expect(task.textContent).not.toContain('TL-1');
  fireEvent.change(task, { target: { value: '01JB000000000000000TSK0001' } });
  fireEvent.change(workstream, { target: { value: '01JB000000000000000WST0003' } });
  expect((task as HTMLSelectElement).value).toBe('');
  expect(task.textContent).not.toContain('PAP-1');
  fireEvent.click(within(dialog).getByRole('button', { name: 'Link session' }));
  await eventually(() => expect(screen.queryByRole('dialog')).toBeNull());
  await eventually(() => expect(screen.getByRole('banner').textContent).toContain('Parsers'));
  expect(requests.filter((r) => r.method === 'POST' && r.path.endsWith('/link')).map((r) => r.body))
    .toEqual([{ workstream: '01JB000000000000000WST0003' }]);
});

it('keeps the dialog and selection after a refused write and allows retry', async () => {
  hub = await startHub();
  let refuse = true;
  renderWithHub(hub, <SessionHeader sessionId={ID.ses5} />, {
    fetch: (input, init) => {
      if (String(input).endsWith('/link') && refuse) {
        refuse = false;
        return Promise.resolve(new Response(JSON.stringify({ code: 'invalid', message: 'Choose again.' }), { status: 400, headers: { 'content-type': 'application/json' } }));
      }
      return fetch(input, init);
    },
  });
  await screen.findByRole('heading', { name: 'Try a cosine schedule' });
  fireEvent.keyDown(screen.getByRole('button', { name: 'Session actions' }), { key: 'Enter' });
  fireEvent.click(await screen.findByRole('menuitem', { name: 'Link to…' }));
  const dialog = await screen.findByRole('dialog', { name: 'Link session' });
  const workstream = within(dialog).getByLabelText('Workstream');
  await eventually(() => expect((workstream as HTMLSelectElement).disabled).toBe(false));
  fireEvent.change(workstream, { target: { value: '01JB000000000000000WST0001' } });
  fireEvent.click(within(dialog).getByRole('button', { name: 'Link session' }));
  expect((await screen.findByRole('alert')).textContent).toContain('Choose again.');
  expect((workstream as HTMLSelectElement).value).toBe('01JB000000000000000WST0001');
  fireEvent.click(within(dialog).getByRole('button', { name: 'Link session' }));
  await eventually(() => expect(screen.queryByRole('dialog')).toBeNull());
});
