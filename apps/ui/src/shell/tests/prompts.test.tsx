// @vitest-environment happy-dom
// @vitest-environment-options {"url": "http://localhost:5173/"}
// SSH's prompts in the desktop app (desktop-gateway.md, "Prompts"): the dialog mounted once at the
// root, over a fake gateway. An answer is sent once and cleared, and is found nowhere afterwards;
// ssh's text is shown as text; Cancel, withdrawal, host keys, and prompts from two hosts queueing.

import { cleanup, fireEvent, render, screen, within } from '@testing-library/react';
import { createMemoryHistory, RouterProvider } from '@tanstack/react-router';
import { afterEach, beforeEach, describe, expect, it, vi } from 'vitest';
import { tinyDaemon } from '../../../tests/fake-gateway.ts';
import { Workspaces } from '../../data/desktop.tsx';
import { createGateway } from '../../data/gateway.ts';
import { FakeDesktop } from '../../data/tests/fake-desktop.ts';
import { WorkspacesContext, type GatewayWorkspace } from '../../data/workspaces.tsx';
import { ENTER_GRACE_MS } from '../prompt-dialog.tsx';
import { createAppRouter } from '../routes.tsx';
import { initialShellState, useShell } from '../store.ts';

const WS = '01JB000000000000000WSPALPH';
const alpha: GatewayWorkspace = { id: WS, name: 'Alpha Lab', kind: 'remote', state: 'ready' };
const SECRET = 'hunter2-correct-horse';
const PATIENCE = { timeout: 8_000 };

let desktop: FakeDesktop;
let registry: Workspaces;
const consoleCalls: unknown[][] = [];

beforeEach(() => {
  desktop = new FakeDesktop({ workspaces: [alpha] }).install();
  desktop.daemons.set(WS, tinyDaemon({ id: WS, name: 'Alpha Lab' }, [{ id: 'PRJA', key: 'ALP', name: 'Alpha project' }]));
  for (const level of ['log', 'info', 'warn', 'error', 'debug'] as const) {
    vi.spyOn(console, level).mockImplementation((...args: unknown[]) => void consoleCalls.push(args));
  }
});

afterEach(() => {
  cleanup();
  registry.stop();
  desktop.uninstall();
  vi.restoreAllMocks();
  consoleCalls.length = 0;
  localStorage.clear();
  sessionStorage.clear();
  useShell.setState(initialShellState);
});

async function renderApp(path = `/w/${WS}/home`) {
  registry = new Workspaces(createGateway());
  registry.start();
  const router = createAppRouter([], { history: createMemoryHistory({ initialEntries: [path] }) });
  render(
    <WorkspacesContext value={registry}>
      <RouterProvider router={router} />
    </WorkspacesContext>,
  );
  await screen.findByRole('link', { name: 'Alpha project' }, PATIENCE);
  return router;
}

const prompt = (id: string, host = 'hpc-login', kind = 'password', text = `sam@${host}'s password: `) => ({
  id,
  host,
  kind,
  text,
});

function dialog(): HTMLElement {
  return screen.getByTestId('gateway-prompt');
}

/** Everything the app could have kept an answer in, as text. */
function everywhere(): string {
  const caches = registry.store
    .getState()
    .list?.map((w) => registry.data(w).queryClient.getQueryCache().getAll().map((q) => q.state.data));
  const storage = (s: Storage) => Object.keys(s).map((k) => `${k}=${s.getItem(k)}`);
  const inputs = [...document.querySelectorAll('input')].map((i) => i.value);
  return JSON.stringify({
    caches,
    local: storage(localStorage),
    session: storage(sessionStorage),
    shell: useShell.getState(),
    prompts: registry.prompts.getState(),
    location: window.location.href,
    html: document.documentElement.outerHTML,
    inputs,
    console: consoleCalls,
  });
}

describe('the prompt dialog', () => {
  it('sends an answer once, clears it, and keeps it nowhere', async () => {
    const router = await renderApp();
    await desktop.prompt(prompt('p1'));
    await screen.findByRole('dialog', { name: 'hpc-login asks for a password' }, PATIENCE);
    expect(within(dialog()).getByTestId('prompt-text').textContent).toBe("sam@hpc-login's password: ");

    const field = within(dialog()).getByLabelText('Password') as HTMLInputElement;
    expect(field.type).toBe('password');
    expect(field.getAttribute('autocomplete')).toBe('off');
    fireEvent.change(field, { target: { value: SECRET } });
    const reply = desktop.nextReply();
    fireEvent.click(within(dialog()).getByRole('button', { name: 'Send' }));

    expect(await reply).toEqual({ id: 'p1', answer: SECRET });
    await vi.waitFor(() => expect(screen.queryByTestId('gateway-prompt')).toBeNull());
    await new Promise((done) => setTimeout(done, 20));
    expect(desktop.commands('gateway_prompt_reply')).toHaveLength(1);
    expect(router.state.location.href).not.toContain(SECRET);
    expect(everywhere()).not.toContain(SECRET);
  });

  it("shows ssh's text as text, never as markup", async () => {
    await renderApp();
    const markup = '<img src=x onerror="alert(1)"><b>Password</b> for <a href="https://example.com">you</a>:';
    await desktop.prompt(prompt('p1', 'hpc-login', 'password', markup));
    await screen.findByTestId('gateway-prompt', undefined, PATIENCE);
    const text = within(dialog()).getByTestId('prompt-text');
    expect(text.textContent).toBe(markup);
    expect(text.children).toHaveLength(0);
    expect(dialog().querySelector('img, b, a')).toBeNull();
  });

  it('never puts a typed answer in the DOM, even before it is sent', async () => {
    await renderApp();
    await desktop.prompt(prompt('p1'));
    await screen.findByTestId('gateway-prompt', undefined, PATIENCE);
    const field = within(dialog()).getByLabelText('Password') as HTMLInputElement;
    fireEvent.change(field, { target: { value: SECRET } });
    expect(field.value).toBe(SECRET);
    expect(field.getAttribute('value')).toBeNull();
    // What DOM snapshots and traces record.
    expect(document.documentElement.outerHTML).not.toContain(SECRET);
    expect((within(dialog()).getByRole('button', { name: 'Send' }) as HTMLButtonElement).disabled).toBe(false);
  });

  it('cancels with neither field, by "Cancel sign-in" or by Esc', async () => {
    await renderApp();
    await desktop.prompt(prompt('p1', 'hpc-login', 'otp', 'Verification code: '));
    await screen.findByRole('dialog', { name: 'hpc-login asks for a one-time code' }, PATIENCE);
    fireEvent.change(within(dialog()).getByLabelText('One-time code'), { target: { value: '123456' } });
    let reply = desktop.nextReply();
    fireEvent.click(within(dialog()).getByRole('button', { name: 'Cancel sign-in' }));
    expect(await reply).toEqual({ id: 'p1' });
    await vi.waitFor(() => expect(screen.queryByTestId('gateway-prompt')).toBeNull());

    await desktop.prompt(prompt('p2', 'hpc-login', 'passphrase', "Enter passphrase for key '/keys/id_ed25519': "));
    await screen.findByRole('dialog', { name: 'Unlock your key to reach hpc-login' });
    expect(within(dialog()).getByRole('button', { name: 'Cancel sign-in' }).getAttribute('aria-keyshortcuts')).toBe('Escape');
    reply = desktop.nextReply();
    fireEvent.keyDown(dialog(), { key: 'Escape' });
    expect(await reply).toEqual({ id: 'p2' });
    await vi.waitFor(() => expect(screen.queryByTestId('gateway-prompt')).toBeNull());
    expect(everywhere()).not.toContain('123456');
  });

  it('ignores a click outside the dialog: a stray click never cancels', async () => {
    await renderApp();
    await desktop.prompt(prompt('p1'));
    await screen.findByTestId('gateway-prompt', undefined, PATIENCE);
    expect(within(dialog()).queryByRole('button', { name: 'Close' })).toBeNull();
    fireEvent.pointerDown(document.body);
    fireEvent.mouseDown(document.body);
    fireEvent.click(document.body);
    fireEvent.focusIn(document.body);
    await new Promise((done) => setTimeout(done, 20));
    expect(screen.getByTestId('gateway-prompt')).toBeTruthy();
    expect(desktop.commands('gateway_prompt_reply')).toEqual([]);
  });

  it('says where each answer goes, from the kind, never from the text', async () => {
    await renderApp();
    await desktop.prompt(prompt('p1', 'hpc-login', 'password', 'Enter your passphrase: '));
    await screen.findByRole('dialog', { name: 'hpc-login asks for a password' }, PATIENCE);
    expect(within(dialog()).getByText('Sent to hpc-login. Not kept here.')).toBeTruthy();
    fireEvent.click(within(dialog()).getByRole('button', { name: 'Cancel sign-in' }));
    await vi.waitFor(() => expect(screen.queryByTestId('gateway-prompt')).toBeNull());

    await desktop.prompt(prompt('p2', 'hpc-login', 'otp', 'Code: '));
    await screen.findByRole('dialog', { name: 'hpc-login asks for a one-time code' });
    expect(within(dialog()).getByText('Sent to hpc-login. Not kept here.')).toBeTruthy();
    fireEvent.click(within(dialog()).getByRole('button', { name: 'Cancel sign-in' }));
    await vi.waitFor(() => expect(screen.queryByTestId('gateway-prompt')).toBeNull());

    await desktop.prompt(prompt('p3', 'hpc-login', 'passphrase', "Enter passphrase for key '/keys/id_ed25519': "));
    await screen.findByRole('dialog', { name: 'Unlock your key to reach hpc-login' });
    expect(within(dialog()).getByText('Unlocks your key on this computer; not sent to hpc-login.')).toBeTruthy();
    expect(within(dialog()).getByRole('region', { name: 'ssh says, for hpc-login' })).toBeTruthy();
  });

  it('keeps a long text in a scroll box, its end shown, and the field and buttons with it', async () => {
    await renderApp();
    const long = `${'Warning: a very long banner from the server. '.repeat(120)}\n(sam@hpc-login) Password: `;
    await desktop.prompt(prompt('p1', 'hpc-login', 'password', long));
    await screen.findByTestId('gateway-prompt', undefined, PATIENCE);
    const box = within(dialog()).getByRole('region', { name: 'hpc-login says' });
    expect(box.className).toContain('max-h-48');
    expect(box.className).toContain('overflow-auto');
    expect(box.getAttribute('tabindex')).toBe('0');
    const text = box.textContent ?? '';
    expect(text.startsWith('…')).toBe(true);
    expect(text.endsWith('(sam@hpc-login) Password: ')).toBe(true);
    expect([...text].length).toBe(2000);
    expect(within(dialog()).getByLabelText('Password')).toBeTruthy();
    expect(within(dialog()).getByRole('button', { name: 'Send' })).toBeTruthy();
  });

  it('ignores Enter for a moment after it opens, so typing meant elsewhere cannot send', async () => {
    await renderApp();
    await desktop.prompt(prompt('p1'));
    await screen.findByTestId('gateway-prompt', undefined, PATIENCE);
    const field = within(dialog()).getByLabelText('Password');
    fireEvent.change(field, { target: { value: 'half-typ' } });
    // `fireEvent` answers false when the event's default action was prevented.
    expect(fireEvent.keyDown(field, { key: 'Enter' })).toBe(false);
    await new Promise((done) => setTimeout(done, ENTER_GRACE_MS + 50));
    expect(fireEvent.keyDown(field, { key: 'Enter' })).toBe(true);
  });

  it('starts each prompt with an empty field (keyed by its id), even of the same kind', async () => {
    await renderApp();
    await desktop.prompt(prompt('p1', 'hpc-login'));
    await desktop.prompt(prompt('p2', 'build-box'));
    await screen.findByRole('dialog', { name: 'hpc-login asks for a password' }, PATIENCE);
    fireEvent.change(within(dialog()).getByLabelText('Password'), { target: { value: SECRET } });
    // Withdrawn, not sent: only the key can make the next field start empty.
    await desktop.closePrompt('p1');
    await screen.findByRole('dialog', { name: 'build-box asks for a password' });
    expect((within(dialog()).getByLabelText('Password') as HTMLInputElement).value).toBe('');
    expect(desktop.commands('gateway_prompt_reply')).toEqual([]);
  });

  it('clears the field the moment it sends', async () => {
    await renderApp();
    await desktop.prompt(prompt('p1'));
    await screen.findByTestId('gateway-prompt', undefined, PATIENCE);
    const field = within(dialog()).getByLabelText('Password') as HTMLInputElement;
    fireEvent.change(field, { target: { value: SECRET } });
    const reply = desktop.nextReply();
    fireEvent.click(within(dialog()).getByRole('button', { name: 'Send' }));
    // The element itself, even once it has left the page, holds nothing.
    expect(field.value).toBe('');
    expect(await reply).toEqual({ id: 'p1', answer: SECRET });
  });

  it('closes a prompt the gateway withdraws, without replying', async () => {
    await renderApp();
    await desktop.prompt(prompt('p1'));
    await screen.findByTestId('gateway-prompt', undefined, PATIENCE);
    fireEvent.change(within(dialog()).getByLabelText('Password'), { target: { value: SECRET } });
    await desktop.closePrompt('p1');
    await vi.waitFor(() => expect(screen.queryByTestId('gateway-prompt')).toBeNull());
    expect(desktop.commands('gateway_prompt_reply')).toEqual([]);
    expect(everywhere()).not.toContain(SECRET);
  });

  it('queues prompts from two hosts, one after the other, each with an empty field', async () => {
    await renderApp();
    await desktop.prompt(prompt('p1', 'hpc-login'));
    await desktop.prompt(prompt('p2', 'build-box', 'otp', 'Code: '));
    await screen.findByRole('dialog', { name: 'hpc-login asks for a password' }, PATIENCE);
    expect(within(dialog()).getByText('One more prompt is waiting.')).toBeTruthy();

    fireEvent.change(within(dialog()).getByLabelText('Password'), { target: { value: SECRET } });
    const first = desktop.nextReply();
    fireEvent.click(within(dialog()).getByRole('button', { name: 'Send' }));
    expect(await first).toEqual({ id: 'p1', answer: SECRET });

    await screen.findByRole('dialog', { name: 'build-box asks for a one-time code' });
    const code = within(dialog()).getByLabelText('One-time code') as HTMLInputElement;
    expect(code.value).toBe('');
    expect(within(dialog()).queryByText(/more prompt/)).toBeNull();
    // Send stays off until there is something to send.
    expect((within(dialog()).getByRole('button', { name: 'Send' }) as HTMLButtonElement).disabled).toBe(true);
    fireEvent.change(code, { target: { value: '424242' } });
    const second = desktop.nextReply();
    fireEvent.submit(code.closest('form') as HTMLFormElement);
    expect(await second).toEqual({ id: 'p2', answer: '424242' });
    await vi.waitFor(() => expect(screen.queryByTestId('gateway-prompt')).toBeNull());
    expect(everywhere()).not.toContain(SECRET);
    expect(everywhere()).not.toContain('424242');
  });

  it('shows a host key with its fingerprint, and accepts or rejects it', async () => {
    await renderApp();
    await desktop.prompt({
      id: 'k1',
      host: 'hpc-login',
      kind: 'host_key',
      text: "The authenticity of host 'hpc-login' can't be established.",
      fingerprint: 'SHA256:bWFkZS11cC1rZXktZm9yLXRlc3RzLW9ubHktMDAwMQ',
    });
    await screen.findByRole('dialog', { name: "Check hpc-login's host key" }, PATIENCE);
    expect(within(dialog()).getByTestId('prompt-fingerprint').textContent).toBe('SHA256:bWFkZS11cC1rZXktZm9yLXRlc3RzLW9ubHktMDAwMQ');
    expect(within(dialog()).queryByRole('textbox')).toBeNull();
    expect(within(dialog()).getByText('Trust this host’s key?')).toBeTruthy();
    let reply = desktop.nextReply();
    fireEvent.click(within(dialog()).getByRole('button', { name: 'Accept' }));
    expect(await reply).toEqual({ id: 'k1', accept: true });

    await desktop.prompt({ id: 'k2', host: 'build-box', kind: 'host_key', text: 'New key.', fingerprint: 'SHA256:abc' });
    await screen.findByRole('dialog', { name: "Check build-box's host key" });
    reply = desktop.nextReply();
    fireEvent.click(within(dialog()).getByRole('button', { name: 'Reject' }));
    expect(await reply).toEqual({ id: 'k2', accept: false });

    // No fingerprint, nothing to compare: it can only be rejected.
    await desktop.prompt({ id: 'k3', host: 'build-box', kind: 'host_key', text: 'New key.' });
    await screen.findByRole('dialog', { name: "Check build-box's host key" });
    expect((within(dialog()).getByRole('button', { name: 'Accept' }) as HTMLButtonElement).disabled).toBe(true);

    // Esc cancels the sign-in, as its button says: neither field.
    reply = desktop.nextReply();
    fireEvent.keyDown(dialog(), { key: 'Escape' });
    expect(await reply).toEqual({ id: 'k3' });
  });

  it("answers ssh's other yes/no questions with Accept or Reject", async () => {
    await renderApp();
    await desktop.prompt({ id: 'c1', host: 'hpc-login', kind: 'confirm', text: 'Accept updated host keys? (yes/no)' });
    await screen.findByRole('dialog', { name: 'ssh asks about hpc-login' }, PATIENCE);
    expect(within(dialog()).getByTestId('prompt-text').textContent).toBe('Accept updated host keys? (yes/no)');
    expect(within(dialog()).queryByRole('textbox')).toBeNull();
    let reply = desktop.nextReply();
    fireEvent.click(within(dialog()).getByRole('button', { name: 'Reject' }));
    expect(await reply).toEqual({ id: 'c1', accept: false });

    await desktop.prompt({ id: 'c2', host: 'hpc-login', kind: 'confirm', text: 'Continue? (yes/no)' });
    await screen.findByRole('dialog', { name: 'ssh asks about hpc-login' });
    reply = desktop.nextReply();
    fireEvent.click(within(dialog()).getByRole('button', { name: 'Accept' }));
    expect(await reply).toEqual({ id: 'c2', accept: true });
  });

  it('shows a notice until it is withdrawn, and only Stop replies (Esc does nothing)', async () => {
    await renderApp();
    await desktop.prompt({ id: 'n1', host: 'hpc-login', kind: 'notice', text: 'Confirm user presence for key ED25519-SK' });
    await screen.findByRole('dialog', { name: 'hpc-login is waiting for you' }, PATIENCE);
    expect(within(dialog()).getByTestId('prompt-text').textContent).toBe('Confirm user presence for key ED25519-SK');
    expect(within(dialog()).getByText('Information only: there is nothing to answer.')).toBeTruthy();
    expect(within(dialog()).queryByRole('textbox')).toBeNull();
    expect(within(dialog()).queryByRole('button', { name: 'Accept' })).toBeNull();
    expect(within(dialog()).queryByRole('button', { name: 'Close' })).toBeNull();
    expect(within(dialog()).queryByRole('button', { name: 'Cancel sign-in' })).toBeNull();
    fireEvent.keyDown(dialog(), { key: 'Escape' });
    fireEvent.pointerDown(document.body);
    await new Promise((done) => setTimeout(done, 20));
    expect(screen.getByTestId('gateway-prompt')).toBeTruthy();
    expect(desktop.commands('gateway_prompt_reply')).toEqual([]);
    // ssh moved on: the gateway withdraws it, and nothing is sent.
    await desktop.closePrompt('n1');
    await vi.waitFor(() => expect(screen.queryByTestId('gateway-prompt')).toBeNull());
    expect(desktop.commands('gateway_prompt_reply')).toEqual([]);

    // The same id again (after a reload, say) shows once, not twice.
    await desktop.prompt({ id: 'n2', host: 'hpc-login', kind: 'notice', text: 'Touch your security key' });
    await desktop.prompt({ id: 'n2', host: 'hpc-login', kind: 'notice', text: 'Touch your security key' });
    await screen.findByRole('dialog', { name: 'hpc-login is waiting for you' });
    expect(within(dialog()).queryByText(/more prompt/)).toBeNull();
    const reply = desktop.nextReply();
    fireEvent.click(within(dialog()).getByRole('button', { name: 'Stop sign-in' }));
    expect(await reply).toEqual({ id: 'n2' });
    await vi.waitFor(() => expect(screen.queryByTestId('gateway-prompt')).toBeNull());
  });
});
