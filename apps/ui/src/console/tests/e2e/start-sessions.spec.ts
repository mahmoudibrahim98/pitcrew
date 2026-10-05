import { expect, test } from '@playwright/test';
import { HUB_TOKEN, HUB_URL } from '../../../../e2e/helpers.ts';

// Also runs against a disposable real daemon through the existing E2E_HUB_URL/TOKEN controls.
test.use({ trace: 'off', video: 'off' });
const ws = '01JB000000000000000WSP0001';
const project = '01JB000000000000000PRJ0001';
const headers = { Authorization: `Bearer ${HUB_TOKEN}` };
const cwd = process.env.E2E_SESSION_CWD ?? '/home/sam/work/start';

for (const entry of ['new', 'list', 'workstream']) {
  test(`starts a session from ${entry} and opens its terminal`, async ({ page, request }) => {
    const machines = await (await request.get(`${HUB_URL}/v1/machines`, { headers })).json();
    const machine = machines.find((m: { kind: string }) => m.kind === 'local');
    const options = await request.get(`${HUB_URL}/v1/machines/${machine.id}/session-options`, { headers });
    expect(options.status()).toBe(200);
    expect((await options.json()).engines.some((e: { engine: string }) => e.engine === 'claude')).toBe(true);
    let path = `/w/${ws}/console`;
    if (entry === 'workstream') {
      const created = await request.post(`${HUB_URL}/v1/workstreams`, { headers, data: {
        project, name: `Synthetic session launch ${Date.now()}`, locations: [{ machine: machine.id, path: cwd }],
      } });
      expect(created.status()).toBe(201);
      path = `/w/${ws}/projects/${project}/workstreams/${(await created.json()).id}`;
    }
    await page.goto(path);
    if (entry === 'new') {
      await page.getByRole('button', { name: 'New', exact: true }).click();
      await page.getByRole('menuitem', { name: 'Session', exact: true }).click();
    } else await page.getByRole('button', { name: 'Start session', exact: true }).click();
    const dialog = page.getByRole('dialog', { name: 'New session' });
    await expect(dialog.getByLabel('Engine')).toBeEnabled();
    if (entry === 'workstream') await expect(dialog.getByLabel('Folder', { exact: true })).toHaveValue(cwd);
    await dialog.getByLabel('Folder', { exact: true }).fill(cwd);
    await dialog.getByLabel('Title (optional)').fill(`Synthetic ${entry} session`);
    const response = page.waitForResponse((r) => r.request().method() === 'POST' && new URL(r.url()).pathname === '/v1/sessions');
    await dialog.getByRole('button', { name: 'Start session', exact: true }).click();
    const started = await response;
    expect(started.status()).toBe(202);
    const session = await started.json();
    try {
      expect(session.terminal).toBeTruthy();
      await expect(page).toHaveURL(new RegExp(`/console/${session.id}\\?view=terminal$`));
      await expect(page.getByRole('radio', { name: 'Terminal', exact: true })).toBeChecked();
      const found = await request.get(`${HUB_URL}/v1/sessions/${session.id}`, { headers });
      expect((await found.json()).terminal).toBe(session.terminal);
    } finally {
      const ended = await request.post(`${HUB_URL}/v1/sessions/${session.id}/end`, { headers, data: { mode: 'kill' } });
      expect(ended.status()).toBe(204);
    }
  });
}
