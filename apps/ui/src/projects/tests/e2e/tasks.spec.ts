import AxeBuilder from '@axe-core/playwright';
import { expect, test } from '@playwright/test';

const WS = '01JB000000000000000WSP0001';
const PAPER = '01JB000000000000000PRJ0001';
const SAM = '01JB000000000000000MEM0001';
const HUB = `http://127.0.0.1:${process.env.E2E_HUB_PORT ?? 47482}`;
const headers = { Authorization: 'Bearer dev-device-token' };
test.use({ trace: 'off', screenshot: 'off', video: 'off' });

test('task drawer edits, traps keyboard focus, archives with Undo and retains a full page', async ({ page, request }) => {
  const response = await request.post(`${HUB}/v1/tasks`, { headers, data: { project: PAPER, title: 'Drawer acceptance', assignee: SAM, due: '2026-10-01' } });
  expect(response.status()).toBe(201);
  const task = await response.json() as { id: string; key: string };
  await page.clock.setFixedTime(new Date('2026-10-01T09:00:00Z'));
  await page.goto(`/w/${WS}/my-tasks`);
  const opener = page.getByRole('button', { name: new RegExp(`${task.key}.*Drawer acceptance`) });
  await opener.focus();
  await page.keyboard.press('Enter');
  const drawer = page.getByRole('dialog', { name: 'Drawer acceptance' });
  await expect(drawer).toBeVisible();
  expect((await new AxeBuilder({ page }).include('[role="dialog"]').analyze()).violations).toEqual([]);
  for (let i = 0; i < 25; i++) {
    await page.keyboard.press('Tab');
    expect(await page.evaluate(() => document.querySelector('[role="dialog"]')?.contains(document.activeElement))).toBe(true);
  }
  await page.keyboard.press('Escape');
  await expect(drawer).not.toBeVisible();
  await expect(opener).toBeFocused();
  await page.keyboard.press('Enter');
  await drawer.getByRole('button', { name: 'Edit task', exact: true }).click();
  await drawer.getByLabel('Title', { exact: true }).fill('Edited in drawer');
  await drawer.getByRole('textbox', { name: 'Description', exact: true }).fill('**Ready** <script>alert(1)</script>');
  await drawer.getByLabel('Priority', { exact: true }).selectOption('high');
  await drawer.getByLabel('Labels', { exact: true }).fill('review\ntests');
  await drawer.getByLabel('Start date', { exact: true }).fill('2026-09-30');
  await drawer.getByRole('button', { name: 'Save task' }).click();
  const edited = page.getByRole('dialog', { name: 'Edited in drawer' });
  await expect(edited.locator('strong')).toHaveText('Ready');
  await expect(edited.locator('script')).toHaveCount(0);
  await edited.getByRole('button', { name: 'Archive task' }).click();
  await expect(edited).not.toBeVisible();
  await expect(page.getByRole('button', { name: new RegExp(`${task.key}.*Edited in drawer`) })).toHaveCount(0);
  // Search leaves the archived task out too (once the results have settled).
  await page.keyboard.press('Control+KeyK');
  await page.getByRole('combobox', { name: 'Search' }).fill('Edited in drawer');
  await expect(page.getByText('No matches.').or(page.getByRole('option').first())).toBeVisible();
  await expect(page.getByRole('option', { name: /Edited in drawer/ })).toHaveCount(0);
  await page.keyboard.press('Control+KeyK');
  await expect(page.getByRole('combobox', { name: 'Search' })).toHaveCount(0);
  await page.getByRole('button', { name: 'Undo', exact: true }).click();
  const restored = page.getByRole('button', { name: new RegExp(`${task.key}.*Edited in drawer`) });
  await expect(restored).toBeVisible();
  await restored.click();
  await edited.getByRole('button', { name: 'Open full page' }).click();
  await expect(page).toHaveURL(new RegExp(`/tasks/${task.id}$`));
  await expect(page.getByRole('dialog')).toHaveCount(0);
  await expect(page.getByRole('heading', { level: 1, name: `${task.key} · Edited in drawer` })).toBeVisible();
});

test('a board column opens the New task dialog with its project and status, and Open stays in the app', async ({ page }) => {
  await page.goto(`/w/${WS}/projects/${PAPER}`);
  await page.getByRole('radio', { name: 'Board', exact: true }).click();
  await expect(page.getByRole('button', { name: /^New task in/ })).toHaveCount(0);
  const button = page.getByRole('button', { name: 'Add a task to In progress', exact: true });
  await expect(button).toBeVisible();
  await button.click();
  const dialog = page.getByRole('dialog', { name: 'New task' });
  await expect(dialog.getByLabel('Project', { exact: true })).toHaveValue(PAPER);
  await expect(dialog.getByLabel('Status', { exact: true })).toHaveValue('in_progress');
  await expect(dialog.getByRole('option', { name: 'Parsers', exact: true })).toHaveCount(0);
  await dialog.getByLabel('Title', { exact: true }).fill('Column defaults');
  await dialog.getByRole('button', { name: 'Create', exact: true }).click();
  await expect(page.getByRole('status').filter({ hasText: /created/ })).toBeVisible();
  // "Open" navigates within the app: the document is not reloaded.
  await page.evaluate(() => { (window as { e2eMarker?: boolean }).e2eMarker = true; });
  await page.getByRole('button', { name: 'Open', exact: true }).click();
  await expect(page).toHaveURL(new RegExp(`/w/${WS}/tasks/[0-9A-Z]{26}$`));
  await expect(page.getByRole('heading', { level: 1, name: /Column defaults/ })).toBeVisible();
  expect(await page.evaluate(() => (window as { e2eMarker?: boolean }).e2eMarker)).toBe(true);
});
