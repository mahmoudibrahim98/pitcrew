import { expect, test, type Page } from '@playwright/test';
import { expectNoAxeViolations } from './axe';
import { FIRST_RUN_HUB, MOCK_DEVICE_TOKEN } from './fresh-hubs';

// The first run in a browser, against a fresh mock hub (`fresh.config.ts`): the shell sends the
// empty workspace to the first-run wizard, the wizard is Welcome, Workspace, Done against the real
// `POST /v1/setup`, and Home stays Home afterwards. `GET /v1/me` is then the new person. axe finds
// nothing on the new screens, light and dark.

const AUTH = { Authorization: `Bearer ${MOCK_DEVICE_TOKEN}` };

function heading(page: Page, name: string | RegExp) {
  return page.getByRole('heading', { level: 1, name });
}

test('the first run, from an empty workspace to Home as the new person', async ({ page, request }) => {
  const before = await request.get(`${FIRST_RUN_HUB}/v1/workspace`, { headers: AUTH });
  const { workspace, setup_needed } = (await before.json()) as { workspace: { id: string }; setup_needed?: boolean };
  expect(setup_needed).toBe(true);
  expect((await request.get(`${FIRST_RUN_HUB}/v1/me`, { headers: AUTH })).status()).toBe(404);
  const ws = workspace.id;

  // `/` opens the workspace, which sends it to setup, shown bare.
  await page.goto('/');
  await expect(page).toHaveURL(new RegExp(`/w/${ws}/onboarding$`));
  await expect(heading(page, 'Welcome to PitCrew')).toBeVisible();
  await expect(page.getByRole('complementary', { name: 'Sidebar' })).toHaveCount(0);
  // Only the steps the hub can serve.
  await expect(page.getByRole('tab')).toHaveText([/Welcome/, /Workspace/, /Done/]);
  await expectNoAxeViolations(page, 'welcome');

  // Any other page goes back to setup while it is not done.
  await page.goto(`/w/${ws}/inbox`);
  await expect(page).toHaveURL(new RegExp(`/w/${ws}/onboarding$`));
  await page.getByRole('button', { name: 'Get started' }).click();

  // Workspace: refused by field first, then sent.
  await expect(heading(page, 'Your first workspace')).toBeVisible();
  await page.getByLabel('Workspace name').fill('   ');
  await page.getByRole('button', { name: 'Continue' }).click();
  await expect(page.getByText('Give the workspace a name.')).toBeVisible();
  await expect(page.getByText('Enter your name.')).toBeVisible();
  await expect(page.getByLabel('Workspace name')).toBeFocused();
  await expectNoAxeViolations(page, 'workspace, refused');

  await page.getByLabel('Workspace name').fill('  Demo Lab ');
  await page.getByLabel('Your name').fill('Sam Rivera');
  await expect(page.getByLabel('Your handle')).toHaveValue('@sam');
  await page.getByLabel('This machine’s name').fill('This laptop');
  await expectNoAxeViolations(page, 'workspace');
  await page.getByRole('button', { name: 'Continue' }).click();

  // Done, then Home, which stays Home.
  await expect(heading(page, "You're set up")).toBeVisible();
  await expect(page.getByText('“Demo Lab” is ready, with you as Sam Rivera (@sam) on This laptop.')).toBeVisible();
  await expectNoAxeViolations(page, 'done');
  await page.getByRole('button', { name: 'Go to Home' }).click();
  await expect(page).toHaveURL(new RegExp(`/w/${ws}/home$`));
  await expect(heading(page, 'Home')).toBeVisible();
  await expect(page.getByTestId('me')).toHaveText('Sam Rivera');

  const me = await request.get(`${FIRST_RUN_HUB}/v1/me`, { headers: AUTH });
  expect(me.status()).toBe(200);
  expect(await me.json()).toMatchObject({ kind: 'human', name: 'Sam Rivera', handle: '@sam' });
  const after = (await (await request.get(`${FIRST_RUN_HUB}/v1/workspace`, { headers: AUTH })).json()) as {
    workspace: { name: string };
    setup_needed?: boolean;
  };
  expect(after.workspace.name).toBe('Demo Lab');
  expect(after.setup_needed).toBeUndefined();

  // A fresh start lands on Home, not on setup.
  await page.goto('/');
  await expect(page).toHaveURL(new RegExp(`/w/${ws}/home$`));
  await expect(heading(page, 'Home')).toBeVisible();
});
