// The Orchestrator panel in a browser, against the hub this run talks to: a question answered with
// links that open their routes in the app, Esc stopping an answer, and clearing the history.
import { expect, test, type Page } from '@playwright/test';
import { expectNoAxeViolations } from './axe';
import { HUB_AUTH, HUB_URL } from './helpers';

const orchestrator = (page: Page) => page.getByRole('complementary', { name: 'Orchestrator' });
const field = (page: Page) => orchestrator(page).getByRole('textbox', { name: 'Ask the Orchestrator' });

async function openPanel(page: Page) {
  await page.goto('/');
  await expect(page).toHaveURL(/\/w\/[^/]+\/home$/);
  await expect(page.getByTestId('count-my-tasks')).toBeVisible();
  await page.keyboard.press('Control+KeyJ');
  await expect(orchestrator(page).getByText('Ask about your work')).toBeVisible();
}

test.afterEach(async ({ request }) => {
  // The specs share one hub: leave no conversation behind.
  await request.delete(`${HUB_URL}/v1/orchestrator/conversations`, { headers: HUB_AUTH });
});

test('"What did my agents do today?" is answered with links that open in the app', async ({ page }) => {
  await openPanel(page);
  await field(page).fill('What did my agents do today?');
  await field(page).press('Enter');
  const conversation = orchestrator(page).getByRole('list', { name: 'Conversation' });
  await expect(conversation.getByText('What did my agents do today?')).toBeVisible();
  await expect(orchestrator(page).getByText(/^Answered in/)).toBeVisible({ timeout: 15_000 });

  const session = conversation.locator('a[href*="/console/"]').first();
  await expect(session).toBeVisible();
  await expect(conversation.locator('a[href*="/tasks/"]').first()).toBeVisible();
  await expect(conversation.getByText('Suggestion:')).toHaveCount(0);
  await expect(conversation.getByRole('group', { name: 'Suggestions' })).toBeVisible();
  await expectNoAxeViolations(page, 'the Orchestrator with an answer');

  const href = (await session.getAttribute('href')) ?? '';
  await session.click();
  await expect(page).toHaveURL(new RegExp(`${href.replace(/[.*+?^${}()|[\]\\]/g, '\\$&')}$`));
  // The panel stays open, with the conversation, on the new page.
  await expect(conversation.getByText('What did my agents do today?')).toBeVisible();
});

test('Esc stops an answer under way; the history clears after asking', async ({ page }) => {
  await openPanel(page);
  await field(page).fill('What is blocked?');
  await field(page).press('Enter');
  await expect(orchestrator(page).getByRole('status')).toBeVisible();
  await field(page).press('Escape');
  await expect(orchestrator(page).getByText(/^Stopped after/)).toBeVisible({ timeout: 15_000 });

  await orchestrator(page).getByRole('button', { name: 'History' }).click();
  await page.getByRole('menuitem', { name: 'Clear history…' }).click();
  const dialog = page.getByRole('dialog');
  await expect(dialog).toBeVisible();
  await dialog.getByRole('button', { name: 'Clear history' }).click();
  await expect(orchestrator(page).getByText('Ask about your work')).toBeVisible();
});
