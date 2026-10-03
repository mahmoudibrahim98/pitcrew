import AxeBuilder from '@axe-core/playwright';
import { expect, test } from '@playwright/test';

const WS = '01JB000000000000000WSP0001';
const PAPER = '01JB000000000000000PRJ0001';
const PAP1 = '01JB000000000000000TSK0001';
const PAP2 = '01JB000000000000000TSK0002';
const HUB = `http://127.0.0.1:${process.env.E2E_HUB_PORT ?? 47482}`;
const headers = { Authorization: 'Bearer dev-device-token' };

// No screenshots or traces on the maintainer's machine. Browser contexts are temporary.
test.use({ trace: 'off', video: 'off' });
test.beforeEach(async ({ page, request }) => {
  await page.clock.setFixedTime(new Date('2026-10-01T09:00:00Z'));
  expect((await request.patch(`${HUB}/v1/tasks/${PAP1}`, { headers, data: { due: '2026-10-10' } })).ok()).toBe(true);
  expect((await request.patch(`${HUB}/v1/tasks/${PAP2}`, { headers, data: { due: null } })).ok()).toBe(true);
  const task = await (await request.get(`${HUB}/v1/tasks/${PAP1}`, { headers })).json() as { status: string };
  if (task.status !== 'in_progress') {
    expect((await request.post(`${HUB}/v1/tasks/${PAP1}/move`, { headers, data: { to: 'in_progress' } })).ok()).toBe(true);
  }
});

for (const theme of ['light', 'dark'] as const) {
  test(`Calendar keyboard, filters, drawer and axe in ${theme}`, async ({ page, request }) => {
    await page.goto(`/w/${WS}/calendar`);
    await page.getByRole('radio', { name: theme === 'light' ? 'Light' : 'Dark', exact: true }).click();
    await expect(page.locator('html')).toHaveAttribute('data-theme', theme);
    await expect(page.getByRole('heading', { level: 1, name: 'Calendar' })).toBeVisible();
    await page.getByLabel('Project', { exact: true }).selectOption(PAPER);
    const day = page.locator('[data-day="2026-10-10"]');
    await expect(day).toHaveAccessibleName(/1 tasks/);
    await day.focus();
    await page.keyboard.press('ArrowLeft');
    await expect(page.locator('[data-day="2026-10-09"]')).toBeFocused();
    await page.keyboard.press('ArrowRight');
    await expect(day).toBeFocused();
    await page.keyboard.press('Enter');
    const selected = page.getByRole('region', { name: 'Selected day tasks' });
    await selected.getByRole('button', { name: /PAP-1/ }).click();
    await expect(page.getByRole('dialog')).toBeVisible();
    await page.getByRole('button', { name: 'Close', exact: true }).click();
    await expect(selected.getByRole('button', { name: /PAP-1/ })).toBeFocused();
    expect((await request.patch(`${HUB}/v1/tasks/${PAP1}`, { headers, data: { due: '2026-10-11' } })).ok()).toBe(true);
    await expect(page.locator('[data-day="2026-10-11"]')).toHaveAccessibleName(/1 tasks/);
    await expect(day).toHaveAccessibleName(/0 tasks/);
    expect((await new AxeBuilder({ page }).analyze()).violations).toEqual([]);
    await page.getByRole('button', { name: 'Collapse sidebar', exact: true }).click();
    await page.setViewportSize({ width: 390, height: 844 });
    await expect(page.locator('.calendar-month')).toBeHidden();
    const agenda = page.getByRole('region', { name: 'Days with tasks' });
    await expect(agenda).toBeVisible();
    await expect(agenda.getByRole('button', { name: /PAP-1/ })).toBeVisible();
    expect((await new AxeBuilder({ page }).analyze()).violations).toEqual([]);
    // Scope to the page: the existing shell toolbar extends 10px past a 390px viewport.
    expect(await page.locator('#main').evaluate((element) => element.scrollWidth <= element.clientWidth)).toBe(true);
  });

  test(`Timeline placement, live status, zoom, contained scrolling and axe in ${theme}`, async ({ page, request }) => {
    await page.goto(`/w/${WS}/projects/${PAPER}`);
    await page.getByRole('radio', { name: theme === 'light' ? 'Light' : 'Dark', exact: true }).click();
    await page.getByRole('radio', { name: 'Timeline', exact: true }).click();
    const timeline = page.getByRole('region', { name: 'Project timeline' });
    await expect(timeline.getByRole('rowheader', { name: 'Submission' })).toBeVisible();
    await expect(timeline.getByRole('rowheader', { name: 'Seed runs' })).toBeVisible();
    await expect(timeline.getByRole('region', { name: 'Tasks without a due date' }).getByRole('button', { name: /PAP-2/ })).toBeVisible();
    await expect(timeline.getByRole('columnheader', { name: /Today/ })).toBeVisible();
    expect((await request.post(`${HUB}/v1/tasks/${PAP1}/move`, { headers, data: { to: 'done' } })).ok()).toBe(true);
    await expect(timeline.locator(`[data-scheduled-task="${PAP1}"]`)).toHaveAttribute('data-status', 'done');
    await timeline.getByRole('button', { name: 'Months', exact: true }).click();
    await expect(timeline.getByRole('columnheader', { name: /Oct 2026/ })).toBeVisible();
    expect((await new AxeBuilder({ page }).analyze()).violations).toEqual([]);
    await page.getByRole('button', { name: 'Collapse sidebar', exact: true }).click();
    await page.setViewportSize({ width: 390, height: 844 });
    await expect(timeline.getByRole('region', { name: 'Timeline time axis' })).toBeVisible();
    expect(await timeline.locator('.timeline-scroll').evaluate((element) => element.scrollWidth > element.clientWidth)).toBe(true);
    expect(await page.locator('#main').evaluate((element) => element.scrollWidth <= element.clientWidth)).toBe(true);
    expect((await new AxeBuilder({ page }).analyze()).violations).toEqual([]);
  });
}
