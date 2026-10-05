// Disposable real hub + synthetic CLI for the three session entry points, on every desktop OS.
import { mkdtemp, mkdir, readdir, readFile, rm, writeFile } from 'node:fs/promises';
import { tmpdir } from 'node:os';
import { delimiter, join, resolve } from 'node:path';
import { spawn, execFileSync } from 'node:child_process';
import { once } from 'node:events';
import { randomBytes } from 'node:crypto';
import { setTimeout as delay } from 'node:timers/promises';

const repo = resolve(import.meta.dirname, '../../../../../..');
const temporary = await mkdtemp(join(tmpdir(), 'pcss-'));
const mark = `pitcrew-session-ui-${randomBytes(12).toString('hex')}`;
const windows = process.platform === 'win32';
const endpoint = windows ? `\\\\.\\pipe\\${mark}` : join(temporary, 'ptyd');
const target = resolve(repo, process.env.CARGO_TARGET_DIR ?? 'target', 'debug');
const binary = (name) => join(target, name + (windows ? '.exe' : ''));
let daemon, browser;
async function stop(child) {
  if (!child?.pid || child.exitCode !== null) return;
  const ended = once(child, 'exit');
  child.kill('SIGTERM');
  const timer = setTimeout(() => child.kill('SIGKILL'), 5000);
  try { await ended; } finally { clearTimeout(timer); }
}
async function marked() {
  const want = `PITCREW_SESSION_UI_RUN=${mark}`;
  if (process.platform === 'linux') {
    const pids = [];
    for (const name of await readdir('/proc')) {
      if (!/^\d+$/.test(name) || Number(name) === process.pid) continue;
      const env = await readFile(`/proc/${name}/environ`).catch(() => null);
      if (env?.toString('latin1').split('\0').includes(want)) pids.push(Number(name));
    }
    return pids;
  }
  if (process.platform === 'darwin') {
    return execFileSync('ps', ['-axE', '-o', 'pid=,command='], { encoding: 'utf8' })
      .split('\n').filter((line) => line.split(/\s+/).includes(want))
      .map((line) => Number(line.trim().split(/\s+/)[0])).filter((pid) => pid !== process.pid);
  }
  // Windows cannot read another process's environment. Only this run's temp path or named
  // pipe identifies its ptyd and CLI; no user's daemon, terminal or real CLI matches them.
  const quote = (s) => `'${s.replaceAll("'", "''")}'`;
  const query = `Get-CimInstance Win32_Process | Where-Object { $_.ProcessId -ne ${process.pid} -and $_.CommandLine -and ($_.CommandLine.Contains(${quote(temporary)}) -or $_.CommandLine.Contains(${quote(endpoint)})) } | Select-Object -ExpandProperty ProcessId`;
  return execFileSync('powershell.exe', ['-NoLogo', '-NoProfile', '-Command', query], { encoding: 'utf8' })
    .split(/\s+/).filter((pid) => /^\d+$/.test(pid)).map(Number);
}
let cleaning;
function cleanup() {
  cleaning ??= (async () => {
    await stop(browser);
    await stop(daemon);
    for (const pid of await marked()) {
      try { process.kill(pid, 'SIGKILL'); } catch (error) { if (error.code !== 'ESRCH') throw error; }
    }
    await rm(temporary, { recursive: true, force: true });
  })();
  return cleaning;
}
for (const signal of ['SIGINT', 'SIGTERM']) process.once(signal, () => {
  void cleanup().then(() => process.exit(130));
});
try {
  const path = (name) => join(temporary, name);
  for (const name of ['state', 'work', 'homes', 'home', 'bin']) await mkdir(path(name), { mode: 0o700 });
  const script = join(path('bin'), 'synthetic-cli.cjs');
  await writeFile(script, "console.log('SYNTHETIC CLI READY'); setInterval(() => {}, 1000);\n");
  const shellQuote = (s) => `'${s.replaceAll("'", "'\\''")}'`;
  const wrapper = windows ? `@echo off\r\n"${process.execPath}" "${script}"\r\n`
    : `#!/bin/sh\nexec ${shellQuote(process.execPath)} ${shellQuote(script)}\n`;
  await writeFile(join(path('bin'), windows ? 'claude.cmd' : 'claude'), wrapper, { mode: 0o700 });
  const env = { ...process.env, HOME: path('home'), USERPROFILE: path('home'),
    CLAUDE_CONFIG_DIR: join(path('homes'), '.claude'), CODEX_HOME: join(path('homes'), '.codex'),
    XDG_DATA_HOME: join(path('home'), 'data'), XDG_CONFIG_HOME: join(path('home'), 'config'),
    APPDATA: join(path('home'), 'appdata'), LOCALAPPDATA: join(path('home'), 'localappdata'),
    PATH: [path('bin'), process.env.PATH].join(delimiter), PITCREW_SESSION_UI_RUN: mark };
  daemon = spawn(binary('pitcrewd'), ['--state-dir', path('state'), 'serve', '--demo',
    '--listen', 'tcp:127.0.0.1:0', '--homes', path('homes'), '--terminal-runtime', 'pty',
    '--ptyd', binary('pitcrew-ptyd'), '--ptyd-endpoint', endpoint, '--ptyd-idle-exit-ms', '500'],
  { cwd: repo, env, stdio: ['ignore', 'pipe', 'pipe'] });
  let output = '';
  let failure;
  daemon.on('error', (error) => { failure = error; });
  daemon.stdout.on('data', (chunk) => { output += chunk; });
  // Drain private diagnostics without exposing token paths or credential files.
  daemon.stderr.resume();
  const deadline = Date.now() + 60000;
  while (!/pitcrewd listening on (http:\/\/127\.0\.0\.1:\d+)/.test(output)) {
    if (failure) throw failure;
    if (daemon.exitCode !== null) throw new Error(`Disposable daemon exited (${daemon.exitCode})`);
    if (Date.now() > deadline) throw new Error('Disposable daemon readiness timeout');
    await delay(20);
  }
  const url = output.match(/pitcrewd listening on (http:\/\/127\.0\.0\.1:\d+)/)[1];
  const token = (await readFile(join(path('state'), 'device.token'), 'utf8')).trim();
  // UI dependencies and Playwright's browser are installed by setup/CI, never by a test.
  browser = spawn(process.execPath, [join(repo, 'apps/ui/node_modules/@playwright/test/cli.js'),
    'test', '--config', process.env.PITCREW_SESSION_UI_CONFIG ?? 'playwright.config.ts', 'start-sessions.spec.ts'],
  { cwd: join(repo, 'apps/ui'), env: { ...process.env, E2E_HUB_URL: url, E2E_HUB_TOKEN: token,
    E2E_SESSION_CWD: path('work'), PITCREW_SESSION_UI_RUN: mark }, stdio: 'inherit' });
  const [code] = await once(browser, 'exit');
  process.exitCode = code ?? 1;
} finally {
  await cleanup();
}
