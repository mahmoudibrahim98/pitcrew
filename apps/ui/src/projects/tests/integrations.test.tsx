// @vitest-environment happy-dom
// @vitest-environment-options {"url": "http://localhost:5173/"}
// Settings › Integrations and a workstream's links upstream, against a real mock hub (which syncs
// from its recorded fixtures, never the network).
import { fireEvent, screen, waitFor, within } from '@testing-library/react';
import axe from 'axe-core';
import { afterEach, beforeEach, expect, it, vi } from 'vitest';
import { createApi, useWorkstream, type Transport, type Workstream } from '../../data/index.ts';
import { githubWebRoot, integrationClient, narrowerScope, scopesOf, type Integration } from '../integrations/api.ts';
import { IntegrationsPage, syncState } from '../integrations/integrations-page.tsx';
import { LinkEditor, WorkstreamLinks } from '../integrations/workstream-links.tsx';
import { Dialog, DialogContent } from '../../design/index.ts';
import { demo, otherClient, renderWithHub, startHub, stopHub, type Hub } from './harness.tsx';

const SECRET = 'synthetic-ui-secret-0001';

let hub: Hub;
beforeEach(async () => {
  hub = await startHub();
});
afterEach(async () => {
  vi.restoreAllMocks();
  await stopHub(hub);
});

it('connects GitHub through gh and shows the sync’s status', async () => {
  renderWithHub(<IntegrationsPage />, hub);
  await screen.findByText('Nothing connected yet.');
  fireEvent.click(screen.getByRole('button', { name: 'Connect GitHub' }));
  const dialog = await screen.findByRole('dialog');
  fireEvent.change(within(dialog).getByLabelText('Repositories'), { target: { value: 'example-org/demo-repo' } });
  fireEvent.click(within(dialog).getByRole('button', { name: 'Connect' }));
  const card = await screen.findByRole('region', { name: 'GitHub' });
  expect(within(card).getByText('example-org/demo-repo')).toBeTruthy();
  expect(within(card).getByText('The GitHub CLI on the hub’s machine')).toBeTruthy();
  await within(card).findByText('In sync');
  // A test reads once and warns that this credential could write.
  fireEvent.click(within(card).getByRole('button', { name: 'Test' }));
  await within(card).findByText('The connection works.');
  expect(within(card).getAllByText(/fine-grained/).length).toBeGreaterThan(0);
});

it('a malformed connection shows the hub’s refusal', async () => {
  renderWithHub(<IntegrationsPage />, hub);
  fireEvent.click(await screen.findByRole('button', { name: 'Connect GitHub' }));
  const dialog = await screen.findByRole('dialog');
  fireEvent.change(within(dialog).getByLabelText('Repositories'), { target: { value: 'not a repo' } });
  fireEvent.click(within(dialog).getByRole('button', { name: 'Connect' }));
  expect((await within(dialog).findByRole('alert')).textContent).toContain('connect GitHub');
});

it('hands a Jira secret over once and never keeps or shows it', async () => {
  const added = await integrationClient(otherClient(hub)).add({
    name: 'Demo Jira',
    settings: { kind: 'jira', deployment: 'cloud', site: 'https://jira.example.com', projects: ['DEMO'], email: 'sam@example.com' },
    credential: 'stored',
  });
  const view = renderWithHub(<IntegrationsPage />, hub);
  const card = await screen.findByRole('region', { name: 'Demo Jira' });
  await within(card).findByText('Needs a credential');
  const field = within(card).getByLabelText('API token') as HTMLInputElement;
  expect(field.type).toBe('password');
  fireEvent.change(field, { target: { value: SECRET } });
  fireEvent.click(within(card).getByRole('button', { name: 'Save' }));
  await within(card).findByText('Saved on the hub.');
  expect(field.value).toBe('');
  await within(card).findByText('Stored on the hub');
  const one = (await integrationClient(otherClient(hub)).list()).find((i) => i.id === added.id);
  expect(one?.credential).toEqual({ source: 'stored', stored: true });
  expect(document.body.innerHTML).not.toContain(SECRET);
  // Nothing in either cache holds it: no query's key or data, and no mutation's variables, data,
  // error or context (a mutation keeps its variables until it is garbage-collected).
  const queries = view.queryClient.getQueryCache().getAll().map((q) => ({ key: q.queryKey, state: q.state }));
  expect(JSON.stringify(queries)).not.toContain(SECRET);
  const mutations = view.queryClient
    .getMutationCache()
    .getAll()
    .map((m) => ({ key: m.options.mutationKey, state: m.state }));
  expect(JSON.stringify(mutations)).not.toContain(SECRET);
});

it('sends a secret through the transport’s own credential call when it has one', async () => {
  const request = vi.fn<Transport['request']>(async () => ({ status: 500, body: '' }));
  const storeCredential = vi.fn<NonNullable<Transport['storeCredential']>>(async () => ({ status: 204, body: '' }));
  const transport: Transport = {
    kind: 'desktop',
    label: 'the workspace “Demo”',
    request,
    openSocket: () => {
      throw new Error('no sockets here');
    },
    storeCredential,
  };
  await integrationClient(createApi({ transport })).storeCredential('01J00000000000000000000000', SECRET);
  expect(storeCredential).toHaveBeenCalledWith('01J00000000000000000000000', SECRET);
  expect(request).not.toHaveBeenCalled();
  storeCredential.mockResolvedValueOnce({ status: 409, body: JSON.stringify({ code: 'conflict', message: 'Keeps no secret.' }) });
  await expect(integrationClient(createApi({ transport })).storeCredential('01J00000000000000000000000', SECRET)).rejects.toThrow(
    'Keeps no secret.',
  );
});

/** The workstream as the page sees it: live, so a link shows once its event arrives. */
function LiveLinks({ id }: { id: string }) {
  const workstream = useWorkstream(id).data;
  return workstream === undefined ? null : <WorkstreamLinks workstream={workstream} />;
}

it('links a workstream to a milestone and shows its last sync', async () => {
  const client = integrationClient(otherClient(hub));
  const added = await client.add({ name: 'GitHub', settings: { kind: 'github', repos: ['example-org/demo-repo'] }, credential: 'gh_cli' });
  renderWithHub(<LiveLinks id={demo.submission} />, hub);
  await screen.findByText('Not linked upstream.');
  fireEvent.click(screen.getByRole('button', { name: 'Edit links' }));
  const dialog = await screen.findByRole('dialog');
  await within(dialog).findByRole('option', { name: 'GitHub repository example-org/demo-repo' });
  fireEvent.change(within(dialog).getByLabelText('Only this milestone (optional)'), { target: { value: '1' } });
  fireEvent.click(within(dialog).getByRole('button', { name: 'Link' }));
  await waitFor(async () => {
    const after: Workstream = await otherClient(hub).workstream(demo.submission);
    expect(after.external.map((l) => l.key)).toEqual(['example-org/demo-repo#milestone:1']);
  });
  await client.sync(added.id);
  fireEvent.click(within(dialog).getByRole('button', { name: 'Done' }));
  await screen.findByRole('link', { name: /GitHub milestone 1 of example-org\/demo-repo/ });
  await screen.findByText(/last sync/);
});

it('links an Enterprise milestone on its own host and checks an epic’s project', async () => {
  const client = integrationClient(otherClient(hub));
  await client.add({
    name: 'Enterprise',
    settings: { kind: 'github', repos: ['example-org/ghe-repo'], api_base: 'https://ghe.example.com/api/v3' },
    credential: 'gh_cli',
  });
  await client.add({
    name: 'Demo Jira',
    settings: { kind: 'jira', deployment: 'data_center', site: 'https://jira.example.com', projects: ['DEMO'] },
    credential: 'stored',
  });
  renderWithHub(<LiveLinks id={demo.submission} />, hub);
  fireEvent.click(await screen.findByRole('button', { name: 'Edit links' }));
  const dialog = await screen.findByRole('dialog');
  await within(dialog).findByRole('option', { name: 'GitHub repository example-org/ghe-repo' });
  const narrower = () => within(dialog).getByRole('textbox');
  fireEvent.change(narrower(), { target: { value: '3' } });
  fireEvent.click(within(dialog).getByRole('button', { name: 'Link' }));
  await waitFor(async () => {
    const after: Workstream = await otherClient(hub).workstream(demo.submission);
    expect(after.external).toEqual([
      { system: 'github', key: 'example-org/ghe-repo#milestone:3', url: 'https://ghe.example.com/example-org/ghe-repo/milestone/3' },
    ]);
  });

  // An epic must be an issue of the chosen project: refused here, before anything is sent.
  const jiraOption = within(dialog).getByRole('option', { name: 'Jira project DEMO' }) as HTMLOptionElement;
  fireEvent.change(within(dialog).getByRole('combobox'), { target: { value: jiraOption.value } });
  fireEvent.change(narrower(), { target: { value: 'OTHER-5' } });
  fireEvent.click(within(dialog).getByRole('button', { name: 'Link' }));
  expect((await within(dialog).findByRole('alert')).textContent).toContain('An epic of DEMO is one of its issue keys, such as DEMO-5.');
  expect((await otherClient(hub).workstream(demo.submission)).external).toHaveLength(1);
  await within(dialog).findByText('GitHub milestone 3 of example-org/ghe-repo');
  fireEvent.change(narrower(), { target: { value: 'demo-5' } });
  fireEvent.click(within(dialog).getByRole('button', { name: 'Link' }));
  await waitFor(async () => {
    const after: Workstream = await otherClient(hub).workstream(demo.submission);
    expect(after.external.map((l) => [l.key, l.url])).toEqual([
      ['example-org/ghe-repo#milestone:3', 'https://ghe.example.com/example-org/ghe-repo/milestone/3'],
      ['DEMO-5', 'https://jira.example.com/browse/DEMO-5'],
    ]);
  });
  // The links on the page point at the Enterprise server, not github.com.
  fireEvent.click(within(dialog).getByRole('button', { name: 'Done' }));
  const shown = await screen.findByRole('link', { name: /GitHub milestone 3 of example-org\/ghe-repo/ });
  expect(shown.getAttribute('href')).toBe('https://ghe.example.com/example-org/ghe-repo/milestone/3');
});

it('builds links from the integration’s own web host', () => {
  expect(githubWebRoot(undefined)).toBe('https://github.com');
  expect(githubWebRoot('https://api.github.com')).toBe('https://github.com');
  expect(githubWebRoot('https://ghe.example.com/api/v3')).toBe('https://ghe.example.com');
  expect(githubWebRoot('https://ghe.example.com:8443/api/v3')).toBe('https://ghe.example.com:8443');
  const enterprise: Integration = {
    id: 'i',
    name: 'n',
    settings: { kind: 'github', repos: ['example-org/demo-repo'], api_base: 'https://ghe.example.com/api/v3' },
    credential: { source: 'gh_cli', stored: false },
    interval_minutes: 15,
    added_by: demo.sam,
    added_at: 0,
    status: { running: false, problems: [] },
    links: [],
  };
  const [repo] = scopesOf(enterprise);
  if (repo === undefined) throw new Error('no repository scope');
  expect(repo.url).toBe('https://ghe.example.com/example-org/demo-repo');
  expect(narrowerScope(enterprise, repo, '12')).toEqual({
    system: 'github',
    key: 'example-org/demo-repo#milestone:12',
    url: 'https://ghe.example.com/example-org/demo-repo/milestone/12',
  });
  expect(typeof narrowerScope(enterprise, repo, 'v1')).toBe('string');
  const jira: Integration = {
    ...enterprise,
    settings: { kind: 'jira', deployment: 'cloud', site: 'https://jira.example.com', projects: ['DEMO'], email: 'sam@example.com' },
  };
  const [project] = scopesOf(jira);
  if (project === undefined) throw new Error('no project scope');
  expect(narrowerScope(jira, project, 'DEMO-5')).toEqual({ system: 'jira', key: 'DEMO-5', url: 'https://jira.example.com/browse/DEMO-5' });
  for (const bad of ['DEMOX-5', 'OTHER-5', 'DEMO-', 'DEMO-0', 'DEMO-5/../x']) {
    expect(typeof narrowerScope(jira, project, bad), bad).toBe('string');
  }
});

it('names a sync’s state', () => {
  const base: Integration = {
    id: 'i',
    name: 'n',
    settings: { kind: 'github', repos: ['example-org/demo-repo'] },
    credential: { source: 'gh_cli', stored: false },
    interval_minutes: 15,
    added_by: demo.sam,
    added_at: 0,
    status: { running: false, problems: [] },
    links: [],
  };
  expect(syncState(base).label).toBe('Not synced yet');
  expect(syncState({ ...base, status: { running: true, problems: [] } }).label).toBe('Syncing');
  expect(syncState({ ...base, status: { running: false, problems: [{ scope: '', message: 'x' }] } }).label).toBe('Problems');
  expect(syncState({ ...base, status: { running: false, problems: [], last_success_at: 1 } }).label).toBe('In sync');
  expect(syncState({ ...base, status: { running: false, problems: [], rate_limited_until: 10_000 } }, 1).tone).toBe('warn');
  expect(syncState({ ...base, credential: { source: 'stored', stored: false } }).label).toBe('Needs a credential');
});

/** axe, as `a11y.test.tsx` runs it (no layout in happy-dom, so no colour contrast). */
async function violations({ modal = false }: { modal?: boolean } = {}): Promise<string[]> {
  const rules: Record<string, { enabled: boolean }> = { 'color-contrast': { enabled: false } };
  if (modal) {
    rules['landmark-one-main'] = { enabled: false };
    rules['page-has-heading-one'] = { enabled: false };
  }
  const result = await axe.run({ exclude: [['[data-radix-focus-guard]']] }, { rules });
  return result.violations.map((v) => `${v.id}: ${v.help} at ${v.nodes.map((n) => JSON.stringify(n.target)).join(', ')}`);
}

it('has no axe violations, connected and while linking', async () => {
  document.title = 'PitCrew';
  document.documentElement.lang = 'en';
  const client = integrationClient(otherClient(hub));
  await client.add({ name: 'GitHub', settings: { kind: 'github', repos: ['example-org/demo-repo'] }, credential: 'gh_cli' });
  await client.add({
    name: 'Demo Jira',
    settings: { kind: 'jira', deployment: 'data_center', site: 'https://jira.example.com', projects: ['DEMO'] },
    credential: 'stored',
  });
  const view = renderWithHub(
    <main>
      <IntegrationsPage />
    </main>,
    hub,
  );
  await screen.findByRole('region', { name: 'Demo Jira' });
  await screen.findByText('Personal access token');
  expect(await violations()).toEqual([]);
  view.unmount();
  // The links dialog as the workstream page opens it, over a page with nothing else to focus.
  const stream: Workstream = await otherClient(hub).workstream(demo.submission);
  renderWithHub(
    <main>
      <h1>Submission</h1>
      <Dialog open>
        <DialogContent title="Links of Submission">
          <LinkEditor workstream={stream} onDone={() => {}} />
        </DialogContent>
      </Dialog>
    </main>,
    hub,
  );
  const dialog = await screen.findByRole('dialog');
  await within(dialog).findByRole('option', { name: 'Jira project DEMO' });
  expect(await violations({ modal: true })).toEqual([]);
});
