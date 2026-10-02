import { expect, test, type Page } from '@playwright/test';
import { expectNoAxeViolations } from './axe';
import { installFakeDesktop, type FakeDesktopRecord } from './fake-desktop';
import { DESKTOP_HUB, MOCK_DEVICE_TOKEN } from './fresh-hubs';

// Connecting a remote machine, in a simulated desktop app (`fake-desktop.ts`: Tauri's internals and
// the gateway, played in the page) whose remote is a fresh mock hub (`fresh.config.ts`). From "No
// workspaces yet" through the connect wizard, SSH's host key and password answered in the app's
// dialog, the fresh remote hub set up through its own transport, to its Home; then the switcher's
// removal dialog. axe finds nothing on any new screen or dialog, light and dark.

const AUTH = { Authorization: `Bearer ${MOCK_DEVICE_TOKEN}` };
const SECRET = 'correct-horse-battery-staple';
const SCRIPT = [
  '#!/bin/bash',
  '#SBATCH --job-name=pitcrewd',
  '#SBATCH --partition=gpu',
  '#SBATCH --cpus-per-task=2',
  '',
  '\texec "$HOME/.pitcrew/bin/current/pitcrewd" serve  ',
  '',
].join('\n');

function heading(page: Page, name: string | RegExp) {
  return page.getByRole('heading', { level: 1, name });
}

function recorded(page: Page): Promise<FakeDesktopRecord> {
  return page.evaluate(() => structuredClone((window as unknown as { __fakeDesktop: FakeDesktopRecord }).__fakeDesktop));
}

test.beforeEach(async ({ page }) => {
  await page.addInitScript(installFakeDesktop, { hubUrl: DESKTOP_HUB, token: MOCK_DEVICE_TOKEN, jobScript: SCRIPT });
});

test('connects a remote machine, answering SSH in the app, and sets its fresh hub up', async ({ page, request }) => {
  await page.goto('/');
  await expect(page.getByText('No workspaces yet.')).toBeVisible();
  await expectNoAxeViolations(page, 'no workspaces');
  await page.getByRole('button', { name: 'Connect a remote machine…' }).click();

  // 1. Host: picked from the ssh config, or typed (and checked).
  await expect(page).toHaveURL(/\/connect$/);
  await expect(heading(page, 'Connect a remote machine')).toBeVisible();
  await expect(page.getByRole('radio', { name: 'hpc-login' })).toBeVisible();
  await expectNoAxeViolations(page, 'host');
  await page.getByLabel('Or type a host').fill('-oProxyCommand=sh');
  await page.getByRole('button', { name: 'Continue' }).click();
  await expect(page.getByRole('alert')).toHaveText('A host cannot start with "-".');
  await expectNoAxeViolations(page, 'host, refused');
  await page.getByRole('radio', { name: 'hpc-login' }).check();
  await page.getByRole('button', { name: 'Continue' }).click();

  // 2. Probe: SSH asks to confirm the host key first.
  const hostKey = page.getByRole('dialog', { name: "Check hpc-login's host key" });
  await expect(hostKey).toBeVisible();
  await expect(hostKey.getByTestId('prompt-fingerprint')).toHaveText('SHA256:bWFkZS11cC1rZXktZm9yLXRlc3RzLW9ubHktMDAwMQ');
  await expectNoAxeViolations(page, 'host key dialog');
  await hostKey.getByRole('button', { name: 'Accept' }).click();
  await expect(heading(page, 'Checking hpc-login')).toBeVisible();
  await expect(page.getByTestId('probe')).toContainText('23.02.7, default partition gpu');
  await expectNoAxeViolations(page, 'probe');
  await page.getByRole('button', { name: 'Continue' }).click();

  // 3. Launcher: SLURM, with its job options.
  await expect(heading(page, 'How PitCrew runs on hpc-login')).toBeVisible();
  await page.getByRole('radio', { name: /As a SLURM job/ }).check();
  await expect(page.getByLabel('Partition')).toHaveValue('gpu');
  await page.getByLabel('CPUs').fill('2');
  await expectNoAxeViolations(page, 'launcher');
  await page.getByRole('button', { name: 'Review the plan' }).click();

  // 4. Review: the exact script, and nothing sent yet.
  await expect(heading(page, 'Review: connect hpc-login')).toBeVisible();
  await expect(page.getByTestId('plan-steps')).toContainText('Submit the job below');
  expect(await page.getByTestId('job-script').textContent()).toBe(SCRIPT);
  await expect(page.getByText('Nothing changes on the remote until you press Connect.')).toBeVisible();
  expect((await recorded(page)).adds).toEqual([]);
  await expectNoAxeViolations(page, 'review');
  await page.getByRole('button', { name: 'Connect' }).click();

  // 5. Connect: SSH asks for a password, answered in the app.
  const password = page.getByRole('dialog', { name: 'hpc-login asks for a password' });
  await expect(password).toBeVisible();
  await expect(password.getByTestId('prompt-text')).toHaveText("sam@hpc-login's password:");
  await expect(password.getByLabel('Password')).toHaveAttribute('type', 'password');
  await expect(password.getByLabel('Password')).toHaveAttribute('autocomplete', 'off');
  await expectNoAxeViolations(page, 'password dialog');
  await password.getByLabel('Password').fill(SECRET);
  await password.getByRole('button', { name: 'Send' }).click();

  // 6. Setup: the fresh remote hub, through its own transport.
  await expect(heading(page, 'Set up hpc-login')).toBeVisible();
  await expect(page.getByLabel('Workspace name')).toHaveValue('hpc-login');
  await expectNoAxeViolations(page, 'setup');
  await page.getByLabel('Workspace name').fill('Cluster Lab');
  await page.getByLabel('Your name').fill('Sam Rivera');
  await page.getByRole('button', { name: 'Set up' }).click();

  // 7. Done: open it.
  await expect(heading(page, 'Connected')).toBeVisible();
  await expectNoAxeViolations(page, 'done');
  await page.getByRole('button', { name: 'Open hpc-login' }).click();
  await expect(page).toHaveURL(/\/w\/[0-9A-Z]{26}\/home$/);
  await expect(heading(page, 'Home')).toBeVisible();
  await expect(page.getByTestId('me')).toHaveText('Sam Rivera');

  const me = await request.get(`${DESKTOP_HUB}/v1/me`, { headers: AUTH });
  expect(await me.json()).toMatchObject({ name: 'Sam Rivera', handle: '@sam' });
  const record = await recorded(page);
  expect(record.adds).toEqual(['plan-1']);
  expect(record.replies).toEqual([
    { id: 'hk-1', answered: false, length: 0, accept: true },
    { id: 'pw-1', answered: true, length: SECRET.length },
  ]);
  // The answer reached the gateway once, and nowhere in the page.
  const leaked = await page.evaluate(
    (secret) =>
      JSON.stringify({ local: { ...localStorage }, session: { ...sessionStorage }, url: location.href, html: document.documentElement.outerHTML }).includes(
        secret,
      ),
    SECRET,
  );
  expect(leaked).toBe(false);

  // The switcher can remove it again, after asking. (It goes by the gateway's name for it.)
  await page.getByRole('button', { name: 'Workspace: hpc-login · login.example.org' }).click();
  await page.getByRole('menuitem', { name: 'Remove workspace…' }).click();
  const remove = page.getByRole('dialog', { name: 'Remove hpc-login · login.example.org?' });
  await expect(remove.getByRole('checkbox', { name: 'Also stop PitCrew on the remote (cancels its SLURM job)' })).toBeVisible();
  await expectNoAxeViolations(page, 'remove dialog');
  await remove.getByRole('button', { name: 'Cancel' }).click();
  await expect(remove).toHaveCount(0);
});
