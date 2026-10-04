import AxeBuilder from '@axe-core/playwright';
import { expect, test } from '@playwright/test';

const session = '01JB000000000000000SES0005';
const submission = '01JB000000000000000WST0001';
const task = '01JB000000000000000TSK0001';

test('links an Unsorted session from its row menu, then the stream moves it into work', async ({ page }) => {
  await page.goto('/w/01JB000000000000000WSP0001/console');
  const list = page.getByRole('listbox', { name: 'Sessions' });
  await expect(list.getByText('Unsorted', { exact: true })).toBeVisible();
  const row = list.locator(`[data-session="${session}"]`);
  await row.click({ button: 'right' });
  await page.getByRole('menuitem', { name: 'Link to…' }).click();
  const dialog = page.getByRole('dialog', { name: 'Link session' });
  await dialog.getByRole('combobox', { name: 'Workstream', exact: true }).selectOption(submission);
  await dialog.getByRole('combobox', { name: 'Task (optional)', exact: true }).selectOption(task);
  expect((await new AxeBuilder({ page }).include('[role="dialog"]').analyze()).violations).toEqual([]);
  const linked = page.waitForResponse((r) => r.url().endsWith(`/v1/sessions/${session}/link`) && r.request().method() === 'POST');
  await dialog.getByRole('button', { name: 'Link session', exact: true }).click();
  const response = await linked;
  expect(response.status()).toBe(200);
  expect(await response.json()).toMatchObject({ workstream: submission, task, link_basis: 'manual' });
  await expect(dialog).toBeHidden();
  await expect(list.getByText('Unsorted', { exact: true })).toHaveCount(0);
  await expect(row).toBeVisible();
  expect((await new AxeBuilder({ page }).analyze()).violations).toEqual([]);
});
