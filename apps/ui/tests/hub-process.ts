// The mock hub as a child process, for tests running under a DOM environment (happy-dom replaces
// globals such as URL that the in-process hub relies on).

import { spawn } from 'node:child_process';
import { createServer } from 'node:net';
import { fileURLToPath } from 'node:url';

const SERVER = fileURLToPath(new globalThis.URL('../../mock-hub/src/server.ts', import.meta.url));
// The tests' pinned clock, so the hub stamps events on the same time line as the UI.
const CLOCK = new globalThis.URL('./clock.ts', import.meta.url).href;

export async function freePort(): Promise<number> {
  const server = createServer();
  await new Promise<void>((done) => server.listen(0, '127.0.0.1', done));
  const address = server.address();
  await new Promise<void>((done) => server.close(() => done()));
  if (address === null || typeof address === 'string') throw new Error('no port');
  return address.port;
}

export interface HubProcess {
  url: string;
  close(): Promise<void>;
}

/** The mock hub on `port`, with `env` added to its environment (`PITCREW_MOCK_*` settings). */
export async function spawnHub(port: number, env: Record<string, string> = {}): Promise<HubProcess> {
  const child = spawn(process.execPath, ['--import', CLOCK, SERVER], {
    env: { ...process.env, ...env, PORT: String(port) },
    stdio: ['ignore', 'pipe', 'inherit'],
  });
  await new Promise<void>((ready, fail) => {
    child.once('exit', (code) => fail(new Error(`mock hub exited with ${code}`)));
    child.stdout.on('data', (chunk: Buffer) => {
      if (chunk.toString().includes('PitCrew mock hub on')) ready();
    });
  });
  child.stdout.resume();
  return {
    url: `http://127.0.0.1:${port}`,
    close: () =>
      new Promise<void>((done) => {
        if (child.exitCode !== null) return done();
        child.once('exit', () => done());
        child.kill();
      }),
  };
}
