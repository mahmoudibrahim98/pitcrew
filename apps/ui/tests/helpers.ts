import { startServer, type RunningServer } from '../../mock-hub/src/server.ts';
import type { SocketFactory, SocketLike } from '../src/data/stream.ts';

export const DEVICE_TOKEN = 'dev-device-token';
export const AGENT_TOKEN = 'dev-agent-token';

export { startServer, type RunningServer };

/** Node's WebSocket, recording the URL of every connection. */
export function recordingSocket(): { factory: SocketFactory; urls: string[] } {
  const urls: string[] = [];
  return {
    urls,
    factory: (url, protocols) => {
      urls.push(url);
      return new WebSocket(url, protocols) as unknown as SocketLike;
    },
  };
}

/** A socket the test drives by hand. */
export class FakeSocket implements SocketLike {
  onmessage: ((message: { data: unknown }) => void) | null = null;
  onclose: (() => void) | null = null;
  onerror: (() => void) | null = null;
  closed = false;
  readonly url: string;
  readonly protocols: string[];

  constructor(url: string, protocols: string[]) {
    this.url = url;
    this.protocols = protocols;
  }

  send(frame: unknown): void {
    this.onmessage?.({ data: JSON.stringify(frame) });
  }

  /** The server went away. */
  drop(): void {
    this.onclose?.();
  }

  close(): void {
    this.closed = true;
  }
}

export function fakeSockets(): { factory: SocketFactory; sockets: FakeSocket[] } {
  const sockets: FakeSocket[] = [];
  return {
    sockets,
    factory: (url, protocols) => {
      const socket = new FakeSocket(url, protocols);
      sockets.push(socket);
      return socket;
    },
  };
}
