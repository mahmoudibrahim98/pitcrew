import { cp, mkdtemp, mkdir, readdir, readFile, rm, writeFile, symlink } from 'node:fs/promises';
import { tmpdir } from 'node:os';
import { delimiter, join, resolve } from 'node:path';
import { execFileSync, spawn } from 'node:child_process';
import { once } from 'node:events';
import { createHash, randomBytes } from 'node:crypto';
import { setTimeout as delay } from 'node:timers/promises';
import { startServer } from '../../apps/mock-hub/src/server.ts';

const target = process.argv[2];
if (!['mock', 'daemon'].includes(target))
  throw new Error('Usage: node tests/conformance/run.mjs mock|daemon');
if (target === 'daemon' && process.platform === 'win32') {
  // Its dispatch starts the agent's CLI in pitcrew-ptyd on a Unix socket, and the stand-in CLIs
  // are shell scripts: on Windows ptyd would look for `.exe`/`.cmd` names on PATH, and could find
  // a real agent CLI. CI runs this target on Linux.
  console.log('# skipped: the daemon target runs on Linux and macOS only');
  process.exit(0);
}
const root = resolve(import.meta.dirname, '../..');
const temporary = await mkdtemp(join(tmpdir(), 'pitcrew-conformance-'));
const home = join(temporary, 'home');
await mkdir(home, { mode: 0o700 });
const env = {
  ...process.env,
  HOME: home,
  USERPROFILE: home,
  CLAUDE_CONFIG_DIR: join(home, ".claude"),
  CODEX_HOME: join(home, ".codex"),
  XDG_DATA_HOME: join(home, 'data'),
  XDG_CONFIG_HOME: join(home, 'config'),
  XDG_CACHE_HOME: join(home, 'cache'),
  XDG_RUNTIME_DIR: join(home, 'runtime'),
  APPDATA: join(home, 'appdata'),
  LOCALAPPDATA: join(home, 'localappdata'),
  PITCREW_CONFORMANCE_EXPECTED: '',
  PITCREW_CONFORMANCE_SYNTHETIC_HOOKS: '1',
};
let daemon, suite, mock, build;
// Every process the daemon starts (pitcrew-ptyd, and the stand-in CLIs it runs) inherits this
// mark, so the cleanup can end them: ptyd keeps a terminal that ended unseen for the next daemon.
const mark = `pitcrew-conformance-${process.pid}-${Date.now()}`;
async function marked() {
  const want = `PITCREW_CONFORMANCE_RUN=${mark}`;
  if (process.platform === 'linux') {
    const pids = [];
    for (const name of await readdir('/proc').catch(() => [])) {
      if (!/^\d+$/.test(name) || Number(name) === process.pid) continue;
      const environ = await readFile(`/proc/${name}/environ`).catch(() => null);
      if (environ?.toString('latin1').split('\0').includes(want)) pids.push(Number(name));
    }
    return pids;
  }
  if (process.platform === 'darwin') {
    try {
      return execFileSync('ps', ['-axE', '-o', 'pid=,command='], { encoding: 'utf8' })
        .split('\n')
        .filter((line) => line.split(/\s+/).includes(want))
        .map((line) => Number(line.trim().split(/\s+/)[0]))
        .filter((pid) => pid !== process.pid);
    } catch {
      return [];
    }
  }
  return [];
}
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
  for (const pid of await marked())
    try {
      process.kill(pid, 'SIGKILL');
    } catch {
      // Already gone.
    }
  await stop(build);
  await mock?.close();
  await rm(temporary, { recursive: true, force: true });
}
for (const signal of ['SIGINT', 'SIGTERM'])
  process.once(signal, () => {
    void cleanup().then(() => process.exit(130));
  });
try {
  const filesRoot = join(temporary, 'files');
  const outside = join(temporary, 'outside');
  await mkdir(join(filesRoot, 'src'), { recursive: true });
  await mkdir(outside);
  await writeFile(join(filesRoot, 'src', 'hello.txt'), 'hello\n');
  await writeFile(join(filesRoot, '.git'), 'synthetic git worktree marker');
  await writeFile(join(filesRoot, '.gitignore'), '*.log\n');
  await writeFile(join(filesRoot, 'debug.log'), 'synthetic ignored file');
  await writeFile(join(filesRoot, 'large.bin'), Buffer.alloc(8 * 1024 * 1024 + 1));
  await writeFile(join(outside, 'secret'), 'synthetic outside');
  try {
    await symlink(outside, join(filesRoot, 'outside'), process.platform === 'win32' ? 'junction' : 'dir');
    env.PITCREW_FILES_LINK = '1';
  } catch (error) {
    if (!['EPERM', 'EACCES'].includes(error.code)) throw error;
    console.log('# skipped: conformance link creation refused by the OS');
  }
  env.PITCREW_FILES_ROOT = filesRoot;
  // Both targets read GitHub and Jira from this copy of the mock hub's recorded fixtures, again at
  // each sync: integrations.test.mjs adds a file that sorts first to change what upstream says.
  const fixtures = join(temporary, 'fixtures');
  await cp(join(root, 'apps', 'mock-hub', 'fixtures'), fixtures, { recursive: true });
  env.PITCREW_CONFORMANCE_FIXTURES = fixtures;
  if (target === 'mock') {
    mock = await startServer({ port: 0, integrationFixtures: fixtures });
    env.PITCREW_CONFORMANCE_URL = mock.url;
    env.PITCREW_CONFORMANCE_PERSON = 'dev-device-token';
    env.PITCREW_CONFORMANCE_AGENT = 'dev-agent-token';
    env.PITCREW_CONFORMANCE_SECOND_PERSON = 'dev-second-device-token';
  } else {
    build = spawn(
      'cargo',
      ['build', '-p', 'pitcrew-daemon', '-p', 'pitcrew-ptyd', '-p', 'pitcrew-cli', '--bins', '--locked'],
      { cwd: root, stdio: 'inherit' },
    );
    const [code] = await once(build, 'exit');
    if (code !== 0) throw new Error('Daemon build failed');
    const state = join(temporary, 'state');
    await mkdir(state, { mode: 0o700 });
    // Provision a second synthetic person's credential before the daemon owns the registry.
    // The suite never prints either credential or opens a real user's registry.
    const second = `pcd_${randomBytes(32).toString('base64url')}`;
    await writeFile(join(state, 'tokens.json'), JSON.stringify({ version: 1, tokens: [{
      id: '01J00000000000000000000001',
      sha256: createHash('sha256').update(second).digest('hex'),
      caller: { member: '01JB000000000000000MEM0007', scope: 'device' },
      created_at: 0,
    }] }), { mode: 0o600 });
    env.PITCREW_CONFORMANCE_SECOND_PERSON = second;
    const refused = join(temporary, 'not-a-socket');
    await writeFile(refused, 'Synthetic runtime refusal\n');
    // A dispatch starts its agent's CLI: stand-ins first on the daemon's PATH, never a real one.
    // Each writes nothing and waits until this run's folder is gone (the cleanup), so its
    // session stays `starting` while the suite runs; pitcrew-ptyd then exits once idle. They
    // answer `--version` at once (a Claude Code new enough for onboarding.test.mjs's hooks), and
    // for machine-setup.test.mjs their status commands too (not signed in); a sign-in's "login"
    // waits like a session.
    const bin = join(temporary, 'bin');
    await mkdir(bin, { mode: 0o700 });
    const standIn =
      `#!/bin/sh\ncase "$1 $2" in\n` +
      `  "--version ") echo 2.1.139; exit 0 ;;\n` +
      `  "auth status"|"login status") echo "Not logged in" >&2; exit 1 ;;\n` +
      `  "auth list") echo "0 credentials"; exit 0 ;;\n` +
      `esac\nwhile [ -d '${state}' ]; do sleep 1; done\n`;
    for (const cli of ['claude', 'codex', 'opencode'])
      await writeFile(join(bin, cli), standIn, { mode: 0o700 });
    // GitHub integrations read `gh auth token`: a stand-in that prints a synthetic credential,
    // never the machine's own gh.
    await writeFile(
      join(bin, 'gh'),
      "#!/bin/sh\n[ \"$1 $2\" = 'auth token' ] || exit 2\necho synthetic-conformance-gh-credential\n",
      { mode: 0o700 },
    );
    env.PATH = [bin, process.env.PATH].filter(Boolean).join(delimiter);
    const ptydEndpoint = join(temporary, 'ptyd');
    await mkdir(ptydEndpoint, { mode: 0o700 });
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
        '--terminal-runtime',
        'pty',
        '--ptyd',
        join(targetDir, 'debug', 'pitcrew-ptyd'),
        '--ptyd-endpoint',
        join(ptydEndpoint, 'ptyd'),
        '--ptyd-idle-exit-ms',
        '500',
        // A scan holds its machine this long, so scan.test.mjs can show a second one refused.
        '--scan-hold-ms',
        '1500',
        // Integrations read the copy of the mock hub's recorded fixtures, never the network.
        '--integration-fixtures',
        fixtures,
      ],
      {
        cwd: root,
        env: { ...env, PITCREW_CONFORMANCE_RUN: mark },
        stdio: ['ignore', 'pipe', 'pipe'],
      },
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
  // One phase per file (or group) that changes the hub for every view, in this order:
  // onboarding.test.mjs first, then the main suite, then import.test.mjs, which commits session
  // inclusion (and restores it), then integrations.test.mjs, which syncs, appending events that
  // the main suite's exact-revision checks must not see, and writes.test.mjs last, on its own: it
  // connects the same repository integrations.test.mjs does. Every phase runs; the first failure
  // decides the exit code.
  for (const files of [
    ['tests/conformance/onboarding.test.mjs'],
    [
      'tests/conformance/api.test.mjs',
      'tests/conformance/scan.test.mjs',
      'tests/conformance/files.test.mjs',
      'tests/conformance/machine-setup.test.mjs',
    ],
    ['tests/conformance/import.test.mjs'],
    ['tests/conformance/integrations.test.mjs'],
    ['tests/conformance/writes.test.mjs'],
  ]) {
    suite = spawn(process.execPath, ['--test', ...files], {
      cwd: root,
      env,
      stdio: 'inherit',
    });
    const [code] = await once(suite, 'exit');
    suite = undefined;
    process.exitCode = process.exitCode || (code ?? 1);
  }
} finally {
  await cleanup();
}
