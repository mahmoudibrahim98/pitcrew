import AxeBuilder from '@axe-core/playwright';
import { expect, test, type Page } from '@playwright/test';

// Runs the whole app (real hub, real dev server — see playwright.config.ts) and drives the actual
// `OnboardingApi` fake wired up in `index.ts`, not a test double. Confirms the acceptance bullets
// in `docs/build/streams/O.md` / the brief: the wizard runs end to end, the Scan → Create step
// produces the ticked projects, and axe finds nothing on any step, light and dark.

async function currentWorkspace(page: Page): Promise<string> {
  await expect(page).toHaveURL(/\/w\/[^/]+\//);
  const match = /\/w\/([^/]+)\//.exec(page.url());
  if (match === null) throw new Error(`No workspace in URL: ${page.url()}`);
  return match[1] ?? '';
}

async function expectNoAxeViolations(page: Page, label: string) {
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
  await page.goto(`/w/${ws}/onboarding`);

  // 1. Welcome. Scope to the step's own panel: the shell's top-bar theme toggle has the same
  // Light/System/Dark labels, so an unscoped query would be ambiguous.
  await expect(heading(page, 'Welcome to PitCrew')).toBeVisible();
  const welcomePanel = page.getByRole('tabpanel', { name: 'Welcome' });
  await welcomePanel.getByRole('radio', { name: theme }).click();
  await expect(page.locator('html')).toHaveAttribute('data-theme', theme.toLowerCase());
  await expectNoAxeViolations(page, `welcome (${theme})`);
  await page.getByRole('button', { name: 'Get started' }).click();

  // 2. Workspace: validates, then a machine.
  await expect(heading(page, 'Your first workspace')).toBeVisible();
  await expectNoAxeViolations(page, `workspace (${theme})`);
  await page.getByRole('button', { name: 'Continue' }).click();
  await expect(page.getByRole('alert')).toContainText(/name/i);
  await page.getByLabel('Workspace name').fill(`Team ${theme}`);
  await page.getByRole('radio', { name: /This computer/ }).click();
  await page.getByRole('button', { name: 'Continue' }).click();

  // 3. Machine check: fix the missing row.
  await expect(heading(page, 'Checking the machine')).toBeVisible();
  const opencodeRow = page.locator('li', { hasText: 'OpenCode CLI' });
  await expect(opencodeRow.getByText('Missing')).toBeVisible();
  await expectNoAxeViolations(page, `machine check (${theme})`);
  await opencodeRow.getByRole('button', { name: 'Fix' }).click();
  await expect(opencodeRow.getByText('OK')).toBeVisible();
  await page.getByRole('button', { name: 'Continue' }).click();

  // 4. Install helper.
  await expect(heading(page, 'Install the helper')).toBeVisible();
  await page.getByRole('button', { name: 'Install the helper' }).click();
  await expect(page.getByText('Helper installed.')).toBeVisible();
  await expectNoAxeViolations(page, `install helper (${theme})`);
  await page.getByRole('button', { name: 'Continue' }).click();

  // 5. Sign in: skip.
  await expect(heading(page, 'Sign in to your agents')).toBeVisible();
  await expect(page.getByText('Claude Code')).toBeVisible();
  await expectNoAxeViolations(page, `sign in (${theme})`);
  await page.getByRole('button', { name: 'Skip for now' }).click();

  // 6. Integrations: skip.
  await expect(heading(page, 'Connect integrations')).toBeVisible();
  await expectNoAxeViolations(page, `integrations (${theme})`);
  await page.getByRole('button', { name: 'Skip integrations' }).click();

  // 7. Scan.
  await expect(heading(page, 'Scanning for sessions')).toBeVisible();
  await expect(page.getByText(/likely project/)).toBeVisible();
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
  await expect(page.getByText(/This will import \d+ sessions?\./)).toBeVisible();
  await expectNoAxeViolations(page, `import (${theme})`);
  await page.getByRole('radio', { name: /Start fresh/ }).click();
  await page.getByRole('button', { name: 'Continue' }).click();

  // 10. Hooks.
  await expect(heading(page, 'Install hooks')).toBeVisible();
  await expect(page.getByText('~/.claude/settings.json')).toBeVisible();
  await expectNoAxeViolations(page, `hooks (${theme})`);
  await page.getByRole('button', { name: 'Install hooks' }).click();

  // 11. Safety.
  await expect(heading(page, 'Safety settings')).toBeVisible();
  await expectNoAxeViolations(page, `safety (${theme})`);
  await page.getByRole('button', { name: 'Continue' }).click();

  // 12. Done: the two suggested projects that stayed ticked were created (Scratch was not).
  await expect(heading(page, "You're set up")).toBeVisible();
  await expect(page.getByText(`Team ${theme}`)).toBeVisible();
  await expect(page.getByText('Created 2 projects.')).toBeVisible();
  await expectNoAxeViolations(page, `done (${theme})`);

  return ws;
}

test('the first-run wizard runs end to end with no axe violations, light theme', async ({ page }) => {
  await walkFirstRun(page, 'Light');
});

test('the first-run wizard runs end to end with no axe violations, dark theme', async ({ page }) => {
  await walkFirstRun(page, 'Dark');
});

test('"Add a machine" opens the shorter wizard from the palette, and it too passes axe', async ({ page }) => {
  await page.goto('/');
  await currentWorkspace(page);
  await page.keyboard.press('Control+KeyK');
  await page.getByRole('combobox', { name: 'Search' }).fill('add a machine');
  await expect(page.getByRole('option').first()).toContainText('Add a machine');
  await page.keyboard.press('Enter');

  await expect(heading(page, 'Add a machine')).toBeVisible();
  await expect(page.getByLabel('Workspace name')).toHaveCount(0);
  await expectNoAxeViolations(page, 'add-machine: machine picker');
  await page.getByRole('radio', { name: /This computer/ }).click();
  await page.getByRole('button', { name: 'Continue' }).click();

  await expect(heading(page, 'Checking the machine')).toBeVisible();
  await page.getByRole('button', { name: 'Continue' }).click();

  await expect(heading(page, 'Install the helper')).toBeVisible();
  await page.getByRole('button', { name: 'Install the helper' }).click();
  await expect(page.getByText('Helper installed.')).toBeVisible();
  await page.getByRole('button', { name: 'Continue' }).click();

  await expect(heading(page, 'Sign in to your agents')).toBeVisible();
  await page.getByRole('button', { name: 'Skip for now' }).click();

  // No integrations step in this flow.
  await expect(heading(page, 'Scanning for sessions')).toBeVisible();
  await expect(page.getByText(/likely project/)).toBeVisible();
  await page.getByRole('button', { name: 'Continue' }).click();

  await expect(heading(page, 'Create projects and workstreams')).toBeVisible();
  await page.getByRole('button', { name: "Don't create any yet" }).click();

  await expect(heading(page, 'Import sessions')).toBeVisible();
  await page.getByRole('button', { name: 'Skip' }).click();

  // No hooks or safety step in this flow: straight to done.
  await expect(heading(page, 'Machine added')).toBeVisible();
  await expect(page.getByRole('button', { name: 'Done' })).toBeVisible();
  await expectNoAxeViolations(page, 'add-machine: done');
});
