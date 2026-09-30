import { expect, test } from '@playwright/test';

test('shows the demo workspace and a move arrives through the stream', async ({ page }, info) => {
  const moves: string[] = [];
  page.on('request', (request) => {
    if (request.method() === 'POST' && request.url().includes('/move')) moves.push(request.url());
  });

  await page.goto('/');
  await expect(page.getByTestId('stream-status')).toHaveText('Live');
  await expect(page.getByRole('heading', { level: 2 }).first()).toBeVisible();
  await expect(page.getByRole('list', { name: 'Live sessions' }).first()).toBeVisible();

  // The first task whose status pill says Todo (not a "→ Todo" button).
  const pill = page.locator('[data-testid^="status-"]', { hasText: /^Todo$/ }).first();
  const key = (await pill.getAttribute('data-testid'))?.replace('status-', '') ?? '';
  expect(key).not.toBe('');
  await expect(page.getByTestId(`status-${key}`)).toHaveText('Todo');

  await page.screenshot({ path: info.outputPath('before-light.png'), fullPage: true });
  await page.getByRole('button', { name: `Move ${key} to In progress` }).click();

  // The move's response does not touch the cache; only the stream's task_moved event does.
  await expect(page.getByTestId(`status-${key}`)).toHaveText('In progress');
  expect(moves).toHaveLength(1);
  await page.screenshot({ path: info.outputPath('after-light.png'), fullPage: true });

  await page.getByRole('radio', { name: 'Dark' }).click();
  await expect(page.locator('html')).toHaveAttribute('data-theme', 'dark');
  await page.screenshot({ path: info.outputPath('after-dark.png'), fullPage: true });
});
