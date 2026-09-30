// @vitest-environment happy-dom
// @vitest-environment-options {"url": "http://localhost:5173/"}

import { cleanup, fireEvent, screen, within } from '@testing-library/react';
import { afterEach, beforeEach, describe, expect, it, vi } from 'vitest';
import { ChatView } from '../chat-view.tsx';
import { Composer } from '../composer.tsx';
import { ID, renderWithHub, startHub, stubLayout, type HubProcess, type Logged } from './harness.tsx';

const posts = (requests: Logged[], path: string) => requests.filter((r) => r.method === 'POST' && r.path === path);

function Pane({ sessionId }: { sessionId: string }) {
  return (
    <>
      <ChatView sessionId={sessionId} />
      <Composer sessionId={sessionId} />
    </>
  );
}

describe('Composer against the mock hub', () => {
  let hub: HubProcess | undefined;
  let unstub: () => void = () => {};

  beforeEach(() => {
    unstub = stubLayout({ viewport: 20_000, row: 40 });
  });

  afterEach(async () => {
    cleanup();
    unstub();
    await hub?.close();
    hub = undefined;
  });

  it('sends on Enter and the canned reply appears in the chat', async () => {
    hub = await startHub();
    const { requests } = renderWithHub(hub, <Pane sessionId={ID.ses4} />);
    await screen.findByText(/The Codex reader now keeps/);
    const input = screen.getByRole('textbox', { name: 'Message to the agent' });
    await vi.waitFor(() => expect(input).toHaveProperty('disabled', false));

    fireEvent.change(input, { target: { value: 'Summarise the parser change\nin two lines' } });
    fireEvent.keyDown(input, { key: 'Enter', shiftKey: true });
    expect(posts(requests, `/v1/sessions/${ID.ses4}/send`)).toHaveLength(0);

    fireEvent.keyDown(input, { key: 'Enter' });
    expect(input).toHaveProperty('value', '');
    // The prompt shows at once, then as the transcript records it, then the reply follows.
    expect(document.querySelectorAll('[data-row="pending"]')).toHaveLength(1);
    await screen.findByText(/Mock reply to "Summarise the parser change in two lines"/, undefined, { timeout: 5_000 });
    expect(posts(requests, `/v1/sessions/${ID.ses4}/send`).map((r) => r.body)).toEqual([
      { text: 'Summarise the parser change\nin two lines' },
    ]);
    await vi.waitFor(() => expect(document.querySelectorAll('[data-row="pending"]')).toHaveLength(0));
    const prompts = [...document.querySelectorAll('[data-row="prompt"]')].map((p) => p.textContent);
    expect(prompts.at(-1)).toContain('Summarise the parser change\nin two lines');
  }, 15_000);

  it('stops a working turn, and sends Escape and Ctrl+C', async () => {
    hub = await startHub();
    const { requests } = renderWithHub(hub, <Pane sessionId={ID.ses1} />);
    const stop = await screen.findByRole('button', { name: 'Stop' });
    await vi.waitFor(() => expect(stop).toHaveProperty('disabled', false));
    fireEvent.click(stop);
    await vi.waitFor(() => expect(posts(requests, `/v1/sessions/${ID.ses1}/interrupt`)).toHaveLength(1));
    // The mock stops the turn: SES0001 now waits, so there is nothing left to stop.
    await vi.waitFor(() => expect(screen.getByRole('button', { name: 'Stop' })).toHaveProperty('disabled', true), {
      timeout: 4_000,
    });

    fireEvent.click(screen.getByRole('button', { name: 'Send Escape' }));
    await vi.waitFor(() => expect(posts(requests, `/v1/sessions/${ID.ses1}/keys`)).toHaveLength(1));
    await vi.waitFor(() => expect(screen.getByRole('button', { name: 'Send Ctrl+C' })).toHaveProperty('disabled', false));
    fireEvent.click(screen.getByRole('button', { name: 'Send Ctrl+C' }));
    await vi.waitFor(() =>
      expect(posts(requests, `/v1/sessions/${ID.ses1}/keys`).map((r) => r.body)).toEqual([
        { keys: ['escape'] },
        { keys: ['ctrl_c'] },
      ]),
    );
    expect(screen.queryByRole('alert')).toBeNull();
  }, 15_000);

  it('is disabled, with the reason, for an ended session and an unreachable one', async () => {
    hub = await startHub();
    renderWithHub(hub, <Composer sessionId={ID.ses6} />);
    const ended = await screen.findByText('This session has ended.');
    const input = screen.getByRole('textbox', { name: 'Message to the agent' });
    expect(input).toHaveProperty('disabled', true);
    expect(input.getAttribute('aria-describedby')).toBe(ended.id);
    cleanup();

    renderWithHub(hub, <Composer sessionId={ID.ses5} />);
    await screen.findByText('gpu-box cannot be reached right now, so the session cannot take input.');
    expect(screen.getByRole('textbox', { name: 'Message to the agent' })).toHaveProperty('disabled', true);
    for (const name of ['Send Escape', 'Send Ctrl+C', 'Stop', 'Send']) {
      expect(screen.getByRole('button', { name })).toHaveProperty('disabled', true);
    }
  });

  it('turns disabled when the hub answers 503, keeping the text', async () => {
    hub = await startHub();
    const unavailable: typeof fetch = (input, init) => {
      const url = new URL(String(input));
      if (init?.method === 'POST' && url.pathname.endsWith('/send')) {
        return Promise.resolve(
          new Response(JSON.stringify({ code: 'unavailable', message: 'This laptop cannot be reached right now.' }), {
            status: 503,
            headers: { 'Content-Type': 'application/json' },
          }),
        );
      }
      return fetch(input, init);
    };
    renderWithHub(hub, <Composer sessionId={ID.ses4} />, { fetch: unavailable });
    const input = screen.getByRole('textbox', { name: 'Message to the agent' });
    await vi.waitFor(() => expect(input).toHaveProperty('disabled', false));
    fireEvent.change(input, { target: { value: 'Are you there?' } });
    fireEvent.keyDown(input, { key: 'Enter' });
    await screen.findByText('This laptop cannot be reached right now.');
    expect(input).toHaveProperty('disabled', true);
    expect(input).toHaveProperty('value', 'Are you there?');
  });

  it('shows a reply quoting hostile text as text', async () => {
    hub = await startHub();
    renderWithHub(hub, <Pane sessionId={ID.ses4} />);
    const input = await screen.findByRole('textbox', { name: 'Message to the agent' });
    await vi.waitFor(() => expect(input).toHaveProperty('disabled', false));
    const payload = '<img src=x onerror=alert(1)> [x](javascript:alert(1))';
    fireEvent.change(input, { target: { value: payload } });
    fireEvent.keyDown(input, { key: 'Enter' });
    const quoted = /Mock reply to "<img src=x onerror=alert\(1\)>/;
    await screen.findByText(quoted, undefined, { timeout: 5_000 });
    await vi.waitFor(() => expect(document.querySelector('[data-markdown="pending"]')).toBeNull());
    const markdown = screen.getByText(quoted).closest('[data-markdown]') as HTMLElement;
    expect(markdown.dataset.markdown).toBe('ready');
    expect(document.querySelectorAll('img, script')).toHaveLength(0);
    expect(within(markdown).queryByRole('link')).toBeNull();
    expect(markdown.textContent).toContain('<img src=x onerror=alert(1)>');
  }, 15_000);
});
