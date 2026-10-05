import { expect, test } from '@playwright/test';
import { expectNoAxeViolations } from '../../../../e2e/axe.ts';
import { EXTERNAL_HUB_URL, HUB_AUTH, HUB_URL } from '../../../../e2e/helpers.ts';

const WS = '01JB000000000000000WSP0001';
const SES = '01JB000000000000000SES0001';

for (const theme of ['light', 'dark'] as const) {
  test(`explorer, hidden entries, quick open, file tabs, breadcrumbs and split in ${theme}`, async ({ page, request }) => {
    const machines = await (await request.get(`${HUB_URL}/v1/machines`, { headers: HUB_AUTH })).json();
    const local = machines.find((machine: { kind: string }) => machine.kind === 'local');
    const created = await request.post(`${HUB_URL}/v1/workstreams`, { headers: HUB_AUTH, data: {
      project: '01JB000000000000000PRJ0001', name: `Synthetic explorer ${theme}`, locations: [{ machine: local.id, path: process.env.E2E_FILES_ROOT ?? '/home/sam/synthetic-explorer' }],
    } });
    expect(created.status()).toBe(201);
    const stream = await created.json();
    await page.goto(`/w/${WS}/projects/01JB000000000000000PRJ0001/workstreams/${stream.id}`);
    await page.getByRole('radio', { name: theme === 'light' ? 'Light' : 'Dark', exact: true }).click();
    const explorer = page.getByRole('complementary', { name: 'File explorer' });
    await expect(explorer.getByRole('button', { name: '▸ src' })).toBeVisible();
    await expect(explorer.getByRole('button', { name: '.git', exact: true })).toHaveCount(0);
    await expect(explorer.getByRole('button', { name: 'debug.log', exact: true })).toHaveCount(0);
    await explorer.getByLabel('Show hidden').check();
    await expect(explorer.getByRole('button', { name: '.git', exact: true })).toBeVisible();
    await expect(explorer.getByRole('button', { name: 'debug.log', exact: true })).toBeVisible();
    await explorer.getByLabel('Show hidden').uncheck();
    await page.keyboard.press('ControlOrMeta+p');
    const quick = page.getByRole('dialog', { name: 'Quick open' });
    await quick.getByLabel('Filename').fill('shello');
    await quick.getByRole('button', { name: 'src/hello.txt' }).click();
    const pane = page.getByRole('region', { name: 'Pane 1', exact: true });
    await expect(pane.getByLabel('File text')).toContainText('hello');
    await expect(pane.getByRole('navigation', { name: 'File breadcrumbs' })).toContainText('src');
    await pane.getByRole('button', { name: 'Copy path' }).click();
    await expect(pane.getByText('Path copied')).toBeVisible();
    await pane.getByRole('button', { name: 'Split right', exact: true }).click();
    await expect(page.getByRole('region', { name: 'Pane 2', exact: true }).getByLabel('File text')).toContainText('hello');
    await expectNoAxeViolations(page, `file explorer and split in ${theme}`);
    await page.reload();
    await expect(page.getByRole('region', { name: 'Pane 2', exact: true }).getByLabel('File text')).toContainText('hello');
  });
}

// The mock transcript is replaced with a synthetic edit receipt; all file I/O still uses the hub.
// A real demo daemon has no transcript ingest fixture, so its run selects the two tests above.
if (EXTERNAL_HUB_URL === undefined) {
  test('one click opens sections/method.tex at the first edited line; diff lines retarget the tab', async ({ page }) => {
    await page.route(`**/v1/sessions/${SES}/transcript*`, async route => {
      const response = await route.fetch();
      const original = await response.json();
      await route.fulfill({ response, json: { ...original, items: [{ kind: 'file_edit', at: 1790757901000, path: 'sections/method.tex', added: 1, removed: 1, diff: '@@ -12,3 +12,3 @@\n context\n context\n-old\n+new', offset: 0 }], at_start: true } });
    });
    await page.goto(`/w/${WS}/console/${SES}`);
    await page.getByRole('button', { name: 'Open sections/method.tex:14', exact: true }).click();
    const pane = page.getByRole('region', { name: 'Pane 1', exact: true });
    await expect(pane.getByLabel('File text')).toHaveAttribute('data-line', '14');
    await expect(pane.getByLabel('File text')).toContainText('Synthetic method line 14');
    await expect(pane.getByLabel('File text')).toHaveJSProperty('scrollTop', 260);
    await expect(pane.getByRole('tablist').getByRole('tab')).toHaveCount(2);
    await pane.getByRole('tab', { name: /Draft method section/ }).click();
    await pane.getByRole('button', { name: /Edited sections\/method.tex/ }).click();
    await pane.getByRole('button', { name: 'Open line 13', exact: true }).click();
    await expect(pane.getByLabel('File text')).toHaveAttribute('data-line', '13');
    await expect(pane.getByRole('tablist').getByRole('tab')).toHaveCount(2);
    await page.getByRole('complementary', { name: 'File explorer' }).getByRole('button', { name: 'Quick open… Ctrl P' }).click();
    const quick = page.getByRole('dialog', { name: 'Quick open' });
    await quick.getByLabel('Filename').fill('method');
    await quick.getByRole('button', { name: 'sections/method.tex' }).click();
    await expect(pane.getByRole('tablist').getByRole('tab')).toHaveCount(2);
    await expect(pane.getByRole('button', { name: 'Copy path' })).toBeVisible();
  });
}
