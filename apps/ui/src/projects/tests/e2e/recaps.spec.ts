import AxeBuilder from '@axe-core/playwright';
import { expect, test, type Page } from '@playwright/test';

// Recaps in the running app against the mock hub (playwright.config.ts in this folder, which runs
// the browser in UTC so the app asks for `tz=0`, the only offset the mock has days for).

const WS = '01JB000000000000000WSP0001';
const PAPER = '01JB000000000000000PRJ0001';
const SEED_RUNS = '01JB000000000000000WST0002';
const SES1 = '01JB000000000000000SES0001';

const SUBMISSION_0930 =
  '2 bursts of work, 1 file edit (+84 −12), 1 task move. @sam dispatched @writer to PAP-1, @writer moved PAP-1 to in progress, updated the plan for PAP-1 (2 of 4 done). @writer edited method.tex (+84 −12).';
const SEED_RUNS_0930 =
  '3 bursts of work, 1 tool run, 1 ask raised. @runner ran a tool. Job 4815164 diverged, @office asked @sam for a decision ("Seed 3 diverged at epoch 9. Rerun or drop it?"). @office marked Seed runs at risk, @sam pinned the brief for Seed runs.';
const SUBMISSION_0929 =
  '@writer finished PAP-3 ("Drafted answers to all 14 comments; 2 need your call."), moved PAP-3 to review.';
const SEED_RUNS_0929 = '@sam dispatched @runner to PAP-4.';

const heading = (page: Page, name: string | RegExp) => page.getByRole('heading', { level: 1, name });
const days = (page: Page) => page.getByRole('list', { name: 'Days' });
const evidence = (page: Page, clause: string) => page.getByRole('dialog', { name: `Evidence for “${clause}”` });

// The dev server compiles the app on its first page load, which can take longer than a test.
test.beforeAll(async ({ browser }) => {
  test.setTimeout(180_000);
  const page = await browser.newPage();
  await page.goto('/', { timeout: 170_000 });
  await expect(heading(page, 'Home')).toBeVisible({ timeout: 60_000 });
  await page.close();
});

/** A project or workstream page, its Activity tab, then Summary. */
async function openSummary(page: Page, path: string, title: string) {
  await page.goto(path);
  await expect(heading(page, title)).toBeVisible();
  await page.getByRole('radio', { name: 'Activity' }).click();
  await page.getByRole('radio', { name: 'Summary' }).click();
  await expect(days(page)).toBeVisible();
}

const projectPath = `/w/${WS}/projects/${PAPER}`;
const openPaperSummary = (page: Page) => openSummary(page, projectPath, 'Paper · Diffusion study');

async function axeViolations(page: Page): Promise<string[]> {
  const result = await new AxeBuilder({ page }).analyze();
  expect(result.passes.length).toBeGreaterThan(20);
  return result.violations.map((v) => `${v.id}: ${v.nodes.map((n) => n.target.join(' ')).join(', ')}`);
}

test("the project's Activity → Summary shows the demo's day paragraphs", async ({ page }, info) => {
  await openPaperSummary(page);
  const list = days(page);
  await expect(list.locator('h3 time')).toHaveCount(2);
  expect(await list.locator('h3 time').evaluateAll((times) => times.map((t) => t.getAttribute('datetime')))).toEqual([
    '2026-09-30',
    '2026-09-29',
  ]);
  await expect(list.getByRole('heading', { level: 4 })).toHaveText(['Submission', 'Seed runs', 'Submission', 'Seed runs']);
  await expect(list.locator('[data-summary]')).toHaveText([SUBMISSION_0930, SEED_RUNS_0930, SUBMISSION_0929, SEED_RUNS_0929]);
  await expect(page.getByText('That’s everything, back to the start.')).toBeVisible();
  await page.screenshot({ path: info.outputPath('project-summary.png'), fullPage: true });

  // A workstream heading leads to the workstream.
  await list.getByRole('heading', { level: 4, name: 'Seed runs' }).first().getByRole('button').click();
  await expect(heading(page, 'Seed runs')).toBeVisible();
});

test('a clause opens its receipts from the keyboard, and leads to the session', async ({ page }, info) => {
  await openPaperSummary(page);
  // Clauses are in the tab order: after the Summary toggle, the first workstream's link, then the
  // first clause of its paragraph.
  await page.getByRole('radio', { name: 'Summary' }).focus();
  await page.keyboard.press('Tab');
  await page.keyboard.press('Tab');
  await expect(days(page).getByRole('button', { name: /^2 bursts of work, with evidence/ }).first()).toBeFocused();

  const name = '@writer moved PAP-1 to in progress';
  const clause = days(page).getByRole('button', { name: new RegExp(`^${name}, with evidence`) });
  await clause.focus();
  // Focus previews the evidence without taking focus.
  await expect(evidence(page, name)).toBeVisible();
  await expect(clause).toBeFocused();
  await expect(clause).toHaveAttribute('aria-expanded', 'false');

  // Enter opens it: focus moves in, Tab reaches its links and receipts.
  await page.keyboard.press('Enter');
  const dialog = evidence(page, name);
  await expect(clause).toHaveAttribute('aria-expanded', 'true');
  await expect(dialog).toBeFocused();
  await expect(dialog.getByRole('list', { name: 'Receipts' })).toHaveText('Event …0005');
  const session = dialog.getByRole('button', { name: 'Draft method section' });
  await expect(session).toBeVisible();
  await expect(dialog.getByRole('button', { name: 'PAP-1' })).toBeVisible();
  await page.screenshot({ path: info.outputPath('clause-evidence.png') });
  await page.keyboard.press('Tab');
  await expect(session).toBeFocused();

  // Escape closes it and returns to the clause.
  await page.keyboard.press('Escape');
  await expect(dialog).toHaveCount(0);
  await expect(clause).toBeFocused();
  await expect(clause).toHaveAttribute('aria-expanded', 'false');

  // Space opens it again; the session link goes to the session in the console.
  await page.keyboard.press('Space');
  await expect(dialog).toBeFocused();
  await page.keyboard.press('Tab');
  await page.keyboard.press('Enter');
  await expect(page).toHaveURL(new RegExp(`/console/${SES1}$`));
});

test("a workstream's Summary, with a day's bursts of work", async ({ page }) => {
  await openSummary(page, `/w/${WS}/projects/${PAPER}/workstreams/${SEED_RUNS}`, 'Seed runs');
  await expect(days(page).locator('[data-summary]')).toHaveText([SEED_RUNS_0930, SEED_RUNS_0929]);
  await expect(days(page).getByRole('heading', { level: 4 })).toHaveCount(0);

  // The disclosure, not the paragraph's clause of the same words ("…, with evidence").
  const disclosure = days(page).getByRole('button', { name: '3 bursts of work', exact: true });
  await disclosure.click();
  await expect(disclosure).toHaveAttribute('aria-expanded', 'true');
  const bursts = page.getByRole('list', { name: /^Bursts of work, / });
  await expect(bursts.locator('[data-summary]')).toHaveText([
    '@runner ran a tool',
    'job 4815164 diverged, @office asked @sam for a decision',
    '@office marked Seed runs at risk, @sam pinned the brief for Seed runs',
  ]);
  await expect(bursts.getByText('1 tool run')).toBeVisible();
});

test("a task's page lists its bursts of work", async ({ page }) => {
  await page.goto(`/w/${WS}/tasks/PAP-1`);
  await expect(heading(page, /^PAP-1 · /)).toBeVisible();
  const work = page.getByRole('region', { name: 'Work' });
  await expect(work.locator('[data-summary]')).toHaveText([
    '@writer edited method.tex (+84 −12)',
    '@sam dispatched @writer to PAP-1, @writer moved PAP-1 to in progress, updated the plan for PAP-1 (2 of 4 done)',
  ]);
  await expect(work.getByText('1 file touched (+84 −12)')).toBeVisible();
});

for (const theme of ['light', 'dark'] as const) {
  for (const layout of ['wide', 'narrow'] as const) {
    test(`axe finds no violations in the Summary: ${layout}, ${theme}`, async ({ page }, info) => {
      await page.emulateMedia({ colorScheme: theme, reducedMotion: 'reduce' });
      if (layout === 'narrow') await page.setViewportSize({ width: 700, height: 800 });
      await page.goto(projectPath);
      await page.getByRole('radio', { name: theme === 'light' ? 'Light' : 'Dark' }).click();
      await expect(page.locator('html')).toHaveAttribute('data-theme', theme);
      await openPaperSummary(page);
      await expect(days(page).locator('[data-summary]')).toHaveCount(4);
      // The workstream names are loaded, and the newest day's first bursts of work are open.
      await expect(days(page).getByRole('heading', { level: 4 }).first()).toHaveText('Submission');
      await days(page).getByRole('button', { name: '2 bursts of work', exact: true }).click();
      await expect(page.getByRole('list', { name: /^Bursts of work, / }).locator('[data-summary]')).toHaveCount(2);
      if (process.env.PITCREW_E2E_SCREENSHOTS === '1') await page.screenshot({ path: info.outputPath(`summary-${layout}-${theme}.png`), fullPage: true });
      expect(await axeViolations(page)).toEqual([]);

      // A clause's evidence, open.
      const name = '1 file edit (+84 −12)';
      await days(page).getByRole('button', { name: new RegExp(`^${name.replace(/[()+]/g, '\\$&')}, with evidence`) }).click();
      await expect(evidence(page, name)).toBeFocused();
      await expect(evidence(page, name).getByRole('list', { name: 'Files' })).toBeVisible();
      if (process.env.PITCREW_E2E_SCREENSHOTS === '1') await page.screenshot({ path: info.outputPath(`evidence-${layout}-${theme}.png`) });
      expect(await axeViolations(page)).toEqual([]);
    });
  }
}
