import { mkdtemp, mkdir, readFile, rm, writeFile } from 'node:fs/promises';
import { tmpdir } from 'node:os';
import { join, resolve } from 'node:path';
import { spawn } from 'node:child_process';
import { once } from 'node:events';
import { setTimeout as delay } from 'node:timers/promises';
import { startServer } from '../../apps/mock-hub/src/server.ts';

const target = process.argv[2];
if (!['mock', 'daemon'].includes(target))
  throw new Error('Usage: node tests/conformance/run.mjs mock|daemon');
const root = resolve(import.meta.dirname, '../..');
const temporary = await mkdtemp(join(tmpdir(), 'pitcrew-conformance-'));
const home = join(temporary, 'home');
await mkdir(home, { mode: 0o700 });
const env = {
  ...process.env,
  HOME: home,
  USERPROFILE: home,
  XDG_DATA_HOME: join(home, 'data'),
  XDG_CONFIG_HOME: join(home, 'config'),
  XDG_CACHE_HOME: join(home, 'cache'),
  XDG_RUNTIME_DIR: join(home, 'runtime'),
  APPDATA: join(home, 'appdata'),
  LOCALAPPDATA: join(home, 'localappdata'),
  PITCREW_CONFORMANCE_EXPECTED: '',
};
let daemon, suite, mock, build;
async function stop(child) {
  if (!child || child.exitCode !== null) return;
  const exited = once(child, 'exit');
  child.kill('SIGTERM');
  const timer = setTimeout(() => child.kill('SIGKILL'), 5000);
  try {
    await exited;
  } finally {
    clearTimeout(timer);
  }
}
let stopping = false;
async function cleanup() {
  if (stopping) return;
  stopping = true;
  await stop(suite);
  await stop(daemon);
  await stop(build);
  await mock?.close();
  await rm(temporary, { recursive: true, force: true });
}
for (const signal of ['SIGINT', 'SIGTERM'])
  process.once(signal, () => {
    void cleanup().then(() => process.exit(130));
  });
try {
  if (target === 'mock') {
    mock = await startServer({ port: 0 });
    env.PITCREW_CONFORMANCE_URL = mock.url;
    env.PITCREW_CONFORMANCE_PERSON = 'dev-device-token';
    env.PITCREW_CONFORMANCE_AGENT = 'dev-agent-token';
  } else {
    build = spawn('cargo', ['build', '-p', 'pitcrew-daemon', '--bin', 'pitcrewd', '--locked'], {
      cwd: root,
      stdio: 'inherit',
    });
    const [code] = await once(build, 'exit');
    if (code !== 0) throw new Error('Daemon build failed');
    const state = join(temporary, 'state');
    await mkdir(state, { mode: 0o700 });
    const refused = join(temporary, 'not-a-socket');
    await writeFile(refused, 'Synthetic runtime refusal\n');
    // Where cargo put it: CARGO_TARGET_DIR when set (as parallel worktrees do), else target/.
    const targetDir = process.env.CARGO_TARGET_DIR ? resolve(root, process.env.CARGO_TARGET_DIR) : join(root, 'target');
    daemon = spawn(
      join(targetDir, 'debug', 'pitcrewd'),
      [
        '--state-dir',
        state,
        'serve',
        '--demo',
        '--listen',
        'tcp:127.0.0.1:0',
        '--tmux-socket',
        refused,
        '--ptyd',
        join(temporary, 'missing-ptyd'),
        // A scan holds its machine this long, so scan.test.mjs can show a second one refused.
        '--scan-hold-ms',
        '1500',
      ],
      { cwd: root, env, stdio: ['ignore', 'pipe', 'pipe'] },
    );
    // Keep diagnostic logs private: they may contain token paths; never print token files.
    let output = '';
    let errors = '';
    daemon.stdout.on('data', (chunk) => (output += chunk));
    daemon.stderr.on('data', (chunk) => {
      errors = (errors + chunk).slice(-100000);
    });
    const deadline = Date.now() + 60000;
    while (!/pitcrewd listening on (http:\/\/127\.0\.0\.1:\d+)/.test(output)) {
      if (daemon.exitCode !== null)
        throw new Error(`Daemon exited before ready (${daemon.exitCode})`);
      if (Date.now() > deadline) throw new Error('Daemon readiness timeout');
      await delay(20);
    }
    env.PITCREW_CONFORMANCE_URL = output.match(
      /pitcrewd listening on (http:\/\/127\.0\.0\.1:\d+)/,
    )[1];
    env.PITCREW_CONFORMANCE_PERSON = (await readFile(join(state, 'device.token'), 'utf8')).trim();
    env.PITCREW_CONFORMANCE_AGENT = (
      await readFile(join(state, 'demo-agent.token'), 'utf8')
    ).trim();
    env.PITCREW_CONFORMANCE_EXPECTED = join(root, 'tests/conformance/daemon-deviations.json');
  }
  suite = spawn(
    process.execPath,
    ['--test', 'tests/conformance/api.test.mjs', 'tests/conformance/scan.test.mjs'],
    {
      cwd: root,
      env,
      stdio: 'inherit',
    },
  );
  const [code] = await once(suite, 'exit');
  process.exitCode = code ?? 1;
} finally {
  await cleanup();
}
