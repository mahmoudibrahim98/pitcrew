import AxeBuilder from '@axe-core/playwright';
import { expect, test, type Page } from '@playwright/test';

// The whole first-run wizard against the in-memory fake, the development-only flag
// (`/w/$ws/onboarding?onboarding=fake`): every step runs end to end, Scan → Create produces the
// ticked projects, and axe finds nothing on any step, light and dark. Against the demo hub, which
// is set up already, so the shell sends nobody here by itself; the real first run against a fresh
// hub is `first-run.fresh.ts` (`fresh.config.ts`).

// A dozen-plus axe scans on top of the wizard's own (short, real) delays.
test.setTimeout(90_000);
test.use({ reducedMotion: 'reduce' });

async function currentWorkspace(page: Page): Promise<string> {
  await expect(page).toHaveURL(/\/w\/[^/]+\//);
  const match = /\/w\/([^/]+)\//.exec(page.url());
  if (match === null) throw new Error(`No workspace in URL: ${page.url()}`);
  return match[1] ?? '';
}

async function expectNoAxeViolations(page: Page, label: string) {
  await page.waitForFunction(() => document.getAnimations().every((animation) => animation.playState === 'finished'));
  const result = await new AxeBuilder({ page }).analyze();
  expect(result.violations.map((v) => `${v.id}: ${v.nodes.map((n) => n.target.join(' ')).join(', ')}`), label).toEqual(
    [],
  );
}

function heading(page: Page, name: string | RegExp) {
  return page.getByRole('heading', { level: 1, name });
}

/** Walks the whole first-run wizard, asserting a11y at every step, in the given theme. */
async function walkFirstRun(page: Page, theme: 'Light' | 'Dark') {
  await page.goto('/');
  const ws = await currentWorkspace(page);
  await page.goto(`/w/${ws}/onboarding?onboarding=fake`);
  const wait = { timeout: 10_000 };

  // 1. Welcome. The first run is shown bare: the step's own theme choice is the only one.
  await expect(heading(page, 'Welcome to PitCrew')).toBeVisible(wait);
  await expect(page.getByRole('complementary', { name: 'Sidebar' })).toHaveCount(0);
  const welcomePanel = page.getByRole('tabpanel', { name: 'Welcome' });
  await welcomePanel.getByRole('radio', { name: theme }).click();
  await expect(page.locator('html')).toHaveAttribute('data-theme', theme.toLowerCase());
  await expect(page.locator('html')).toHaveCSS('color-scheme', theme.toLowerCase());
  await expectNoAxeViolations(page, `welcome (${theme})`);
  await page.getByRole('button', { name: 'Get started' }).click();

  // 2. Workspace: validates by field, the handle follows the name.
  await expect(heading(page, 'Your first workspace')).toBeVisible();
  await page.getByRole('button', { name: 'Continue' }).click();
  await expect(page.getByText('Give the workspace a name.')).toBeVisible();
  await expectNoAxeViolations(page, `workspace, refused (${theme})`);
  await page.getByLabel('Workspace name').fill(`Team ${theme}`);
  await page.getByLabel('Your name').fill('Sam Rivera');
  await expect(page.getByLabel('Your handle')).toHaveValue('@sam');
  await expectNoAxeViolations(page, `workspace (${theme})`);
  await page.getByRole('button', { name: 'Continue' }).click();

  // 3. Machine check: fix the missing row.
  await expect(heading(page, 'Checking the machine')).toBeVisible();
  const opencodeRow = page.locator('li', { hasText: 'OpenCode CLI' });
  await expect(opencodeRow.getByText('Missing')).toBeVisible(wait);
  await expectNoAxeViolations(page, `machine check (${theme})`);
  await opencodeRow.getByRole('button', { name: 'Fix' }).click();
  await expect(opencodeRow.getByText('OK')).toBeVisible(wait);
  await page.getByRole('button', { name: 'Continue' }).click();

  // 4. Install helper.
  await expect(heading(page, 'Install the helper')).toBeVisible();
  await page.getByRole('button', { name: 'Install the helper' }).click();
  await expect(page.getByText('Helper installed.')).toBeVisible(wait);
  await expectNoAxeViolations(page, `install helper (${theme})`);
  await page.getByRole('button', { name: 'Continue' }).click();

  // 5. Sign in: skip.
  await expect(heading(page, 'Sign in to your agents')).toBeVisible();
  await expect(page.getByText('Claude Code')).toBeVisible(wait);
  await expectNoAxeViolations(page, `sign in (${theme})`);
  await page.getByRole('button', { name: 'Skip for now' }).click();

  // 6. Integrations: skip.
  await expect(heading(page, 'Connect integrations')).toBeVisible();
  await expectNoAxeViolations(page, `integrations (${theme})`);
  await page.getByRole('button', { name: 'Skip integrations' }).click();

  // 7. Scan.
  await expect(heading(page, 'Scanning for sessions')).toBeVisible();
  await expect(page.getByText(/likely project/)).toBeVisible(wait);
  await expectNoAxeViolations(page, `scan (${theme})`);
  await page.getByRole('button', { name: 'Continue' }).click();

  // 8. Create: untick one suggestion, then create — the ticked ones are what gets created.
  await expect(heading(page, 'Create projects and workstreams')).toBeVisible();
  await expect(page.getByLabel('Include Scratch')).toBeChecked();
  await page.getByLabel('Include Scratch').uncheck();
  await expectNoAxeViolations(page, `create (${theme})`);
  await page.getByRole('button', { name: 'Create', exact: true }).click();

  // 9. Import: start fresh.
  await expect(heading(page, 'Import sessions')).toBeVisible();
  await expect(page.getByText(/This will import \d+ sessions?\./)).toBeVisible(wait);
  await expectNoAxeViolations(page, `import (${theme})`);
  await page.getByRole('radio', { name: /Start fresh/ }).click();
  await page.getByRole('button', { name: 'Continue' }).click();

  // 10. Hooks.
  await expect(heading(page, 'Install hooks')).toBeVisible();
  await expect(page.getByText('~/.claude/settings.json', { exact: true })).toBeVisible(wait);
  await expectNoAxeViolations(page, `hooks (${theme})`);
  await page.getByRole('button', { name: 'Install hooks' }).click();

  // 11. Safety.
  await expect(heading(page, 'Safety settings')).toBeVisible();
  await expectNoAxeViolations(page, `safety (${theme})`);
  await page.getByRole('button', { name: 'Continue' }).click();

  // 12. Done: the two suggested projects that stayed ticked were created (Scratch was not).
  await expect(heading(page, "You're set up")).toBeVisible();
  await expect(page.getByText(`“Team ${theme}” is ready`)).toBeVisible();
  await expect(page.getByText('Created 2 projects.')).toBeVisible();
  await expectNoAxeViolations(page, `done (${theme})`);

  // Home, which the demo hub (set up long ago) does not send back to setup.
  await page.getByRole('button', { name: 'Go to Home' }).click();
  await expect(page).toHaveURL(new RegExp(`/w/${ws}/home$`));
  await expect(heading(page, 'Home')).toBeVisible();
}

test('the first-run wizard runs end to end against the fake, with no axe violations, light theme', async ({ page }) => {
  await walkFirstRun(page, 'Light');
});

test('the first-run wizard runs end to end against the fake, with no axe violations, dark theme', async ({ page }) => {
  await walkFirstRun(page, 'Dark');
});

test('nothing offers the old "Add a machine" wizard any more', async ({ page }) => {
  await page.goto('/');
  await currentWorkspace(page);
  await page.keyboard.press('Control+KeyK');
  await page.getByRole('combobox', { name: 'Search' }).fill('add a machine');
  await expect(page.getByRole('option', { name: /Add a machine/ })).toHaveCount(0);
});
