// @vitest-environment happy-dom
// @vitest-environment-options {"url": "http://localhost:5173/"}

// The Orchestrator panel against the mock hub (a child process): asking, the answer with its links
// to app routes, suggestions that only act when clicked, follow-ups, Esc, clearing the history,
// and the state with no agent CLI to ask with.

import type { QueryClient } from '@tanstack/react-query';
import { createMemoryHistory, RouterProvider, type AnyRouter } from '@tanstack/react-router';
import { cleanup, fireEvent, render, screen, waitFor, within } from '@testing-library/react';
import { afterAll, afterEach, beforeAll, beforeEach, describe, expect, it } from 'vitest';
import { freePort, spawnHub, type HubProcess } from '../../../tests/hub-process.ts';
import { createApi } from '../../data/api.ts';
import { keys } from '../../data/keys.ts';
import type { Orchestrator } from '../../data/orchestrator.ts';
import { createQueryClient, DataProvider } from '../../data/provider.tsx';
import type { SocketFactory } from '../../data/stream.ts';
import { createAppRouter } from '../routes.tsx';
import { initialShellState, useShell } from '../store.ts';

const TOKEN = 'dev-device-token';
const WORKSPACE = '01JB000000000000000WSP0001';
const HOME = `/w/${WORKSPACE}/home`;

let hub: HubProcess;

beforeAll(async () => {
  hub = await spawnHub(await freePort());
});

afterAll(async () => {
  await hub.close();
});

let queryClient: QueryClient | undefined;

beforeEach(() => {
  useShell.setState({ ...initialShellState, orchestratorOpen: true });
});

afterEach(async () => {
  cleanup();
  await queryClient?.cancelQueries();
  queryClient?.clear();
  localStorage.clear();
  useShell.setState(initialShellState);
  // Each test starts with no conversations.
  await fetch(`${hub.url}/v1/orchestrator/conversations`, { method: 'DELETE', headers: { Authorization: `Bearer ${TOKEN}` } });
});

function renderApp(path = HOME, socket?: SocketFactory, seed?: (client: QueryClient) => void): AnyRouter {
  const router = createAppRouter([], { history: createMemoryHistory({ initialEntries: [path] }) });
  const api = createApi({ baseUrl: hub.url, token: TOKEN });
  queryClient = createQueryClient();
  seed?.(queryClient);
  render(
    <DataProvider api={api} queryClient={queryClient} token={TOKEN} {...(socket === undefined ? {} : { socket })}>
      <RouterProvider router={router} />
    </DataProvider>,
  );
  return router;
}

const panel = () => screen.getByRole('complementary', { name: 'Orchestrator' });
/** The panel, once the frame is on screen. */
const shown = () => screen.findByRole('complementary', { name: 'Orchestrator' }, { timeout: 8_000 });
const field = () => within(panel()).getByRole('textbox', { name: 'Ask the Orchestrator' });

async function ask(text: string) {
  await shown();
  const input = await within(panel()).findByRole('textbox', { name: 'Ask the Orchestrator' }, { timeout: 8_000 });
  await waitFor(() => expect((input as HTMLTextAreaElement).disabled).toBe(false), { timeout: 8_000 });
  fireEvent.change(input, { target: { value: text } });
  fireEvent.keyDown(input, { key: 'Enter' });
}

async function hubGet<T>(path: string): Promise<T> {
  const response = await fetch(hub.url + path, { headers: { Authorization: `Bearer ${TOKEN}` } });
  return (await response.json()) as T;
}

describe('the Orchestrator panel', () => {
  it('answers a question with links to app routes, and suggestions that act only when clicked', async () => {
    const router = renderApp();
    await shown();
    await within(panel()).findByText('Ask about your work', undefined, { timeout: 8_000 });
    // An example fills the field; Enter asks.
    fireEvent.click(await within(panel()).findByRole('button', { name: 'What did my agents do today?' }));
    expect((field() as HTMLTextAreaElement).value).toBe('What did my agents do today?');
    fireEvent.keyDown(field(), { key: 'Enter' });

    const conversation = await within(panel()).findByRole('list', { name: 'Conversation' }, { timeout: 8_000 });
    expect(within(conversation).getByText('What did my agents do today?')).toBeTruthy();
    // While it answers: a status, and Stop in place of Ask.
    expect(within(panel()).getByRole('button', { name: 'Stop' })).toBeTruthy();

    await within(panel()).findByText(/^Answered in/, undefined, { timeout: 8_000 });
    const links = within(conversation).getAllByRole('link');
    const hrefs = links.map((a) => a.getAttribute('href') ?? '');
    expect(hrefs.some((h) => h.startsWith(`/w/${WORKSPACE}/console/`))).toBe(true);
    expect(hrefs.some((h) => h.startsWith(`/w/${WORKSPACE}/tasks/`)), hrefs.join(' ')).toBe(true);
    expect(hrefs.some((h) => h.startsWith(`/w/${WORKSPACE}/projects/`))).toBe(true);
    // The suggestions' lines are not in the answer; they are buttons.
    expect(conversation.textContent).not.toContain('Suggestion:');
    const suggestions = within(conversation).getByRole('group', { name: 'Suggestions' });
    const move = within(suggestions).getByRole('button', { name: /^Move / });
    const task = /^Move (\S+) to/.exec(move.textContent ?? '')?.[1] ?? '';
    expect(task).toMatch(/^[A-Z]+-\d+$/);
    const before = await hubGet<{ status: string }>(`/v1/tasks/${task}`);

    // A move asks first; Cancel moves nothing.
    fireEvent.click(move);
    const confirm = within(suggestions).getByRole('group', { name: 'Confirm' });
    fireEvent.click(within(confirm).getByRole('button', { name: 'Cancel' }));
    expect(within(suggestions).queryByRole('group', { name: 'Confirm' })).toBeNull();
    expect((await hubGet<{ status: string }>(`/v1/tasks/${task}`)).status).toBe(before.status);
    // Confirmed, the person moves it.
    fireEvent.click(move);
    fireEvent.click(within(within(suggestions).getByRole('group', { name: 'Confirm' })).getByRole('button', { name: 'Move' }));
    await waitFor(async () => expect((await hubGet<{ status: string }>(`/v1/tasks/${task}`)).status).toBe('review'));
    await within(suggestions).findByRole('button', { name: /: done$/ });

    // An "open" suggestion opens its route in the app; so does a reference.
    fireEvent.click(within(suggestions).getByRole('button', { name: /^Open / }));
    await waitFor(() => expect(router.state.location.pathname).toMatch(new RegExp(`^/w/${WORKSPACE}/console/`)));
    const [taskLink] = links.filter((a) => (a.getAttribute('href') ?? '').includes('/tasks/'));
    if (taskLink === undefined) throw new Error('no link to a task');
    fireEvent.click(taskLink);
    await waitFor(() => expect(router.state.location.pathname).toBe(taskLink.getAttribute('href')));
  });

  it('types a follow-up into the same conversation, and starts a new one', async () => {
    renderApp();
    await ask('What is blocked?');
    await within(panel()).findByText(/^Answered in/, undefined, { timeout: 8_000 });
    expect(field().getAttribute('placeholder')).toBe('Ask a follow-up…');
    await ask('And today?');
    await within(panel()).findByText('And today?');
    await waitFor(() => expect(within(panel()).getAllByText(/^Answered in/)).toHaveLength(2), { timeout: 8_000 });
    expect(within(panel()).getByText(/^Following up/)).toBeTruthy();

    // New conversation: an empty panel; the old one stays in the history.
    fireEvent.click(within(panel()).getByRole('button', { name: 'New conversation' }));
    expect(within(panel()).getByText('Ask about your work')).toBeTruthy();
    fireEvent.keyDown(within(panel()).getByRole('button', { name: 'History' }), { key: 'Enter' });
    const menu = await screen.findByRole('menu');
    fireEvent.click(within(menu).getByRole('menuitem', { name: 'What is blocked?' }));
    await within(panel()).findByText('And today?');
  });

  it('stops an answer under way with Esc', async () => {
    renderApp();
    await ask('What did my agents do today?');
    await within(panel()).findByRole('status', undefined, { timeout: 8_000 });
    fireEvent.keyDown(field(), { key: 'Escape' });
    await within(panel()).findByText(/^Stopped after/, undefined, { timeout: 8_000 });
    expect(within(panel()).queryByRole('button', { name: 'Stop' })).toBeNull();
    const state = await hubGet<Orchestrator>('/v1/orchestrator');
    expect(state.conversations[0]?.turns[0]?.state).toBe('canceled');
  });

  it('clears the history, after asking', async () => {
    renderApp();
    await ask('What is blocked?');
    await within(panel()).findByText(/^Answered in/, undefined, { timeout: 8_000 });
    fireEvent.keyDown(within(panel()).getByRole('button', { name: 'History' }), { key: 'Enter' });
    fireEvent.click(within(await screen.findByRole('menu')).getByRole('menuitem', { name: 'Clear history…' }));
    const dialog = await screen.findByRole('dialog');
    // Keeping it forgets nothing.
    fireEvent.click(within(dialog).getByRole('button', { name: 'Keep it' }));
    await waitFor(() => expect(screen.queryByRole('dialog')).toBeNull());
    expect((await hubGet<Orchestrator>('/v1/orchestrator')).conversations).toHaveLength(1);

    fireEvent.keyDown(within(panel()).getByRole('button', { name: 'History' }), { key: 'Enter' });
    fireEvent.click(within(await screen.findByRole('menu')).getByRole('menuitem', { name: 'Clear history…' }));
    fireEvent.click(within(await screen.findByRole('dialog')).getByRole('button', { name: 'Clear history' }));
    await within(panel()).findByText('Ask about your work');
    await waitFor(async () => expect((await hubGet<Orchestrator>('/v1/orchestrator')).conversations).toEqual([]));
  });

  it('says when no agent CLI can answer, and links to signing in', async () => {
    const none: Orchestrator = {
      engines: [
        { engine: 'claude', installed: false },
        { engine: 'codex', installed: false },
        { engine: 'opencode', installed: false },
      ],
      limits: { question_chars: 4000, answer_bytes: 16384, answer_seconds: 300, turns: 20, conversations: 20 },
      conversations: [],
    };
    // A stream that never says hello: the seeded state is all the panel sees.
    const silent: SocketFactory = () => ({ onmessage: null, onclose: null, onerror: null, close: () => undefined });
    renderApp(HOME, silent, (client) => client.setQueryData(keys.orchestrator, none));
    await shown();
    await within(panel()).findByText('No agent CLI to ask with', undefined, { timeout: 8_000 });
    const link = within(panel()).getByRole('link', { name: 'Install one and sign in' });
    expect(link.getAttribute('href')).toBe(`/w/${WORKSPACE}/sign-in`);
    expect((field() as HTMLTextAreaElement).disabled).toBe(true);
  });
});
