// @vitest-environment happy-dom
// @vitest-environment-options {"url": "http://localhost:5173/"}
// In the desktop app the webview holds no token (ADR-0003): the app as it starts (`AppData`),
// with a fake gateway, must never read the browser's config or VITE_PITCREW_* values, never use
// fetch or WebSocket, and never send a token, an `Authorization` header or a bearer subprotocol
// through the gateway, for requests, the stream or a terminal.

import { cleanup, render, screen } from '@testing-library/react';
import { createMemoryHistory, createRoute, RouterProvider } from '@tanstack/react-router';
import { StrictMode, useEffect, useState } from 'react';
import { afterEach, beforeEach, describe, expect, it, vi } from 'vitest';
import { AppData, terminalPath, useOpenSocket } from '../src/data/index.ts';
import { defineFeature } from '../src/shell/feature.ts';
import { createAppRouter } from '../src/shell/routes.tsx';
import { initialShellState, useShell } from '../src/shell/store.ts';
import { FakeGateway, helloOnStream, tinyDaemon } from './fake-gateway.ts';

const config = vi.hoisted(() => ({ reads: 0 }));

vi.mock('../src/data/config.ts', async (importOriginal) => {
  const actual = await importOriginal<typeof import('../src/data/config.ts')>();
  return {
    ...actual,
    browserConfig: () => {
      config.reads += 1;
      return actual.browserConfig();
    },
    resolveApi: (...args: Parameters<typeof actual.resolveApi>) => {
      config.reads += 1;
      return actual.resolveApi(...args);
    },
    resolveToken: (...args: Parameters<typeof actual.resolveToken>) => {
      config.reads += 1;
      return actual.resolveToken(...args);
    },
  };
});

const WS = '01JB000000000000000WSP0001';
const SECRET = 'secret-token-from-env';

/** A page that opens a terminal through the data layer's export, types, and resizes. */
const terminal = defineFeature({
  id: 'terminal-probe',
  layout: 'console',
  routes: (parent) => [
    createRoute({
      getParentRoute: () => parent,
      path: 'terminal-probe',
      component: function TerminalProbe() {
        const open = useOpenSocket();
        const [output, setOutput] = useState('');
        useEffect(() => {
          const socket = open(terminalPath('01JB000000000000000SES0001', { cols: 80, rows: 24 }));
          socket.onmessage = (message) => {
            if (message.data instanceof ArrayBuffer) setOutput(new TextDecoder().decode(message.data));
          };
          socket.send(new TextEncoder().encode('ls\r'));
          socket.send(JSON.stringify({ type: 'resize', cols: 100, rows: 30 }));
          return () => socket.close();
        }, [open]);
        return <h1>Terminal: {output}</h1>;
      },
    }),
  ],
});

let gateway: FakeGateway;
const fetchSpy = vi.fn(() => Promise.reject(new TypeError('fetch is not for the desktop app')));
const webSocketSpy = vi.fn(() => {
  throw new Error('WebSocket is not for the desktop app');
});

beforeEach(() => {
  vi.stubEnv('VITE_PITCREW_TOKEN', SECRET);
  vi.stubEnv('VITE_PITCREW_API', 'http://127.0.0.1:1');
  vi.stubGlobal('fetch', fetchSpy);
  vi.stubGlobal('WebSocket', webSocketSpy);
  gateway = new FakeGateway([{ id: WS, name: 'Demo Lab', kind: 'local', state: 'ready' }]).install();
  gateway.daemons.set(WS, tinyDaemon({ id: WS, name: 'Demo Lab' }, [{ id: 'PRJ1', key: 'PAP', name: 'Paper' }]));
  helloOnStream(gateway);
  const echo = gateway.onSocket;
  gateway.onSocket = (socket) => {
    echo?.(socket);
    if (socket.path.includes('/terminal')) socket.binary([...new TextEncoder().encode('hello from the shell')]);
  };
});

afterEach(() => {
  cleanup();
  gateway.uninstall();
  vi.unstubAllEnvs();
  vi.unstubAllGlobals();
  localStorage.clear();
  useShell.setState(initialShellState);
});

describe('the desktop app holds no token', () => {
  it('reads no config or token, uses no fetch or WebSocket, and sends no token through the gateway', async () => {
    const router = createAppRouter([terminal], { history: createMemoryHistory({ initialEntries: ['/'] }) });
    render(
      <StrictMode>
        <AppData>
          <RouterProvider router={router} />
        </AppData>
      </StrictMode>,
    );
    await screen.findByRole('link', { name: 'Paper' }, { timeout: 8_000 });
    await router.navigate({ href: `/w/${WS}/terminal-probe` });
    await screen.findByRole('heading', { name: 'Terminal: hello from the shell' });
    const terminals = () => gateway.sockets.filter((s) => s.path.includes('/terminal'));
    await vi.waitFor(() => expect(terminals().at(-1)?.sent).toHaveLength(2));
    expect(terminals().at(-1)?.path).toBe('/v1/sessions/01JB000000000000000SES0001/terminal?cols=80&rows=24');

    expect(config.reads).toBe(0);
    expect(fetchSpy).not.toHaveBeenCalled();
    expect(webSocketSpy).not.toHaveBeenCalled();

    // Requests, the stream and the terminal all went through the gateway, by its commands only.
    const commands = new Set(gateway.calls.map((c) => c.cmd));
    for (const cmd of ['gateway_workspaces', 'gateway_request', 'gateway_socket_open', 'gateway_socket_send']) {
      expect(commands).toContain(cmd);
    }
    for (const cmd of commands) {
      expect(['gateway_workspaces', 'gateway_request', 'gateway_socket_open', 'gateway_socket_send', 'gateway_socket_close']).toContain(cmd);
    }
    expect(gateway.sockets.map((s) => s.path)).toContain('/v1/stream');

    for (const { args } of gateway.calls) {
      const text = JSON.stringify(args).toLowerCase();
      for (const word of [SECRET, 'dev-device-token', 'authorization', 'bearer', 'vite_pitcrew', 'token']) {
        expect(text).not.toContain(word.toLowerCase());
      }
    }
    // A request is the contract's four fields; the webview sets no headers.
    for (const call of gateway.calls.filter((c) => c.cmd === 'gateway_request')) {
      for (const key of Object.keys(call.args.req as object)) expect(['workspace', 'method', 'path', 'body']).toContain(key);
    }
  });
});
