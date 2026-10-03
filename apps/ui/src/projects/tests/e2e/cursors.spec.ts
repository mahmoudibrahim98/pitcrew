import AxeBuilder from '@axe-core/playwright';
import { expect, test } from '@playwright/test';

const WS = '01JB000000000000000WSP0001';
const PAPER = '01JB000000000000000PRJ0001';
const STREAM = '01JB000000000000000WST0001';
const HUB = `http://127.0.0.1:${process.env.E2E_HUB_PORT ?? 47482}`;
const headers = { Authorization: 'Bearer dev-device-token' };

for (const theme of ['light', 'dark'] as const) {
  test(`read cursors follow devices and scopes with axe in ${theme}`, async ({ page, request }) => {
    test.setTimeout(120_000);
    await page.goto(`/w/${WS}`);
    await expect(page.getByRole('heading', { level: 1, name: 'Home' })).toBeVisible();
    await page.getByRole('radio', { name: theme === 'light' ? 'Light' : 'Dark', exact: true }).click();
    const region = page.getByRole('region', { name: 'Since you last looked' });
    // Ensure this test has a new change even when earlier tests advanced the cursor.
    const created = await request.post(`${HUB}/v1/projects`, { headers, data: { name: `Cursor ${theme}`, key: theme === 'light' ? 'CUR' : 'CRD' } });
    expect(created.ok()).toBe(true);
    await expect(region.getByText(`created the project Cursor ${theme}`)).toBeVisible();
    await expect(region.getByRole('button', { name: 'Mark all as read' })).toBeVisible();
    await region.getByRole('button', { name: 'Mark all as read' }).click();
    await expect(region.getByText('Nothing new since you last looked.')).toBeVisible();
    await page.reload();
    await expect(region.getByText('Nothing new since you last looked.')).toBeVisible();
    const another = await request.post(`${HUB}/v1/projects`, { headers, data: { name: `Live ${theme}`, key: theme === 'light' ? 'CUL' : 'CDL' } });
    expect(another.ok()).toBe(true);
    await expect(region.getByText('1 new change')).toBeVisible();
    const workspace = await (await request.get(`${HUB}/v1/workspace`, { headers })).json() as { rev: number };
    expect((await request.put(`${HUB}/v1/me/cursors/workspace`, { headers, data: { rev: workspace.rev } })).ok()).toBe(true);
    await expect(region.getByText('Nothing new since you last looked.')).toBeVisible();
    expect((await new AxeBuilder({ page }).analyze()).violations).toEqual([]);
    for (const [path, scope, title] of [[`projects/${PAPER}`, `project:${PAPER}`, 'Paper · Diffusion study'], [`projects/${PAPER}/workstreams/${STREAM}`, `workstream:${STREAM}`, 'Submission']]) {
      await page.goto(`/w/${WS}/${path}`);
      await expect(page.getByRole('main')).toBeVisible();
      await expect(page.getByRole('heading', { level: 1, name: title, exact: true })).toBeVisible();
      await expect.poll(async () => {
        const cursors = await (await request.get(`${HUB}/v1/me/cursors`, { headers })).json() as { scope: string; rev: number }[];
        return cursors.find((c) => c.scope === scope)?.rev ?? 0;
      }).toBeGreaterThanOrEqual(workspace.rev);
      expect((await new AxeBuilder({ page }).analyze()).violations).toEqual([]);
    }
  });
}
