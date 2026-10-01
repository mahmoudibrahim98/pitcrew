// @vitest-environment happy-dom
// @vitest-environment-options {"url": "http://localhost:5173/"}

import { cleanup, render, screen } from '@testing-library/react';
import { afterEach, describe, expect, it, vi } from 'vitest';
import { createApi } from '../src/data/api.ts';
import { useProjects, useWorkspace } from '../src/data/hooks.ts';
import { createQueryClient, DataProvider, useConnection } from '../src/data/provider.tsx';
import type { SocketFactory, SocketLike } from '../src/data/stream.ts';
import { freePort, spawnHub, type HubProcess } from './hub-process.ts';

const DEVICE_TOKEN = 'dev-device-token';

function Projects() {
  const projects = useProjects();
  const workspace = useWorkspace();
  const { status } = useConnection();
  return (
    <div>
      <p>status: {status}</p>
      <p>workspace: {workspace.data?.workspace.name ?? '-'}</p>
      <ul>
        {projects.data?.map((p) => (
          <li key={p.id}>{p.name}</li>
        ))}
      </ul>
    </div>
  );
}

/** Records when the stream's first frame arrived and when each request started. */
function recorder() {
  const log: string[] = [];
  const socket: SocketFactory = (url, protocols) => {
    log.push('stream:open');
    const ws = new WebSocket(url, protocols) as unknown as SocketLike;
    const wrapped: SocketLike = {
      onmessage: null,
      onclose: null,
      onerror: null,
      close: () => ws.close(),
    };
    ws.onmessage = (message) => {
      const frame = JSON.parse(String(message.data)) as { type: string };
      log.push(`stream:${frame.type}`);
      wrapped.onmessage?.(message);
    };
    ws.onclose = () => wrapped.onclose?.();
    ws.onerror = () => wrapped.onerror?.();
    return wrapped;
  };
  const fetcher: typeof fetch = (input, init) => {
    log.push(`GET ${new URL(String(input)).pathname}`);
    return fetch(input, init);
  };
  return { log, socket, fetcher };
}

describe('DataProvider', () => {
  let hub: HubProcess | undefined;

  afterEach(async () => {
    cleanup();
    await hub?.close();
    hub = undefined;
  });

  it('runs no data query before the stream says hello', async () => {
    hub = await spawnHub(await freePort());
    const { log, socket, fetcher } = recorder();
    const api = createApi({ baseUrl: hub.url, token: DEVICE_TOKEN, fetch: fetcher });
    render(
      <DataProvider api={api} queryClient={createQueryClient()} token={DEVICE_TOKEN} socket={socket}>
        <Projects />
      </DataProvider>,
    );
    await screen.findByText('Tooling');
    const hello = log.indexOf('stream:hello');
    const firstGet = log.findIndex((line) => line.startsWith('GET '));
    expect(hello).toBeGreaterThanOrEqual(0);
    expect(firstGet).toBeGreaterThan(hello);
    expect(log).toContain('GET /v1/projects');
  });

  it('recovers without a reload when the hub starts after the UI', async () => {
    // A free port with nothing listening on it yet.
    const port = await freePort();

    const baseUrl = `http://127.0.0.1:${port}`;
    // Until the hub runs, requests (the connection probe) fail as a refused connection would.
    // happy-dom's fetch would print each refusal to the real stderr, past vitest.
    let listening = false;
    const fetcher: typeof fetch = (input, init) =>
      listening ? fetch(input, init) : Promise.reject(new TypeError('fetch failed'));
    const api = createApi({ baseUrl, token: DEVICE_TOKEN, fetch: fetcher });
    render(
      <DataProvider api={api} queryClient={createQueryClient()} token={DEVICE_TOKEN}>
        <Projects />
      </DataProvider>,
    );
    await screen.findByText('status: reconnecting');
    await new Promise((r) => setTimeout(r, 1_000));
    expect(screen.getByText('workspace: -')).toBeTruthy();

    hub = await spawnHub(port);
    listening = true;
    await screen.findByText('Tooling', undefined, { timeout: 8_000 });
    await vi.waitFor(() => expect(screen.getByText('status: live')).toBeTruthy());
    await vi.waitFor(() => expect(screen.queryByText('workspace: -')).toBeNull());
  }, 15_000);
});
