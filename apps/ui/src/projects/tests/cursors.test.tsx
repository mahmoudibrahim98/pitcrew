import { screen, within } from '@testing-library/react';
import { afterEach, beforeEach, expect, it } from 'vitest';
import { Home } from '../home.tsx';
import { ReadScope } from '../read-scope.tsx';
import { demo, eventually, otherClient, renderWithHub, startHub, stopHub, type Hub } from './harness.tsx';

let hub: Hub;
beforeEach(async () => { hub = await startHub(); });
afterEach(async () => { await stopHub(hub); });

it('a cursor moved on another device refreshes Home without reloading', async () => {
  renderWithHub(<Home />, hub);
  const region = await screen.findByRole('region', { name: 'Since you last looked' });
  await within(region).findByText('15 new changes');
  await otherClient(hub).request('PUT', '/v1/me/cursors/workspace', { body: { rev: 15 } });
  await within(region).findByText('Nothing new since you last looked.');
  await otherClient(hub).moveTask('PAP-2', 'in_progress');
  await within(region).findByText('1 new change');
});

it('leaving a scope before the dwell does not mark it read', async () => {
  const view = renderWithHub(<ReadScope scope={`project:${demo.paper}`} />, hub);
  await eventually(() => expect(view.queryClient.getQueryData(['cursors'])).toEqual([]));
  view.unmount();
  await new Promise((done) => setTimeout(done, 1100));
  expect(await otherClient(hub).request('GET', '/v1/me/cursors')).toEqual([]);
});

it('marks a visited scope once and leaves later live changes unread', async () => {
  const scope = `workstream:${demo.submission}`;
  renderWithHub(<ReadScope scope={scope} />, hub);
  await eventually(async () => expect(await otherClient(hub).request('GET', '/v1/me/cursors')).toEqual([{ scope, rev: 15 }]));
  await otherClient(hub).moveTask('PAP-2', 'in_progress');
  await new Promise((done) => setTimeout(done, 1100));
  expect(await otherClient(hub).request('GET', '/v1/me/cursors')).toEqual([{ scope, rev: 15 }]);
});
