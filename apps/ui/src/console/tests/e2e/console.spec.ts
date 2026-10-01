import AxeBuilder from '@axe-core/playwright';
import { expect, test, type Page } from '@playwright/test';

// The Agent console's acceptance, in the running app against the mock hub (playwright.config.ts
// in this folder). The specs run in order on one hub: the later ones send a prompt and answer a
// question, which change their sessions.

const WS = '01JB000000000000000WSP0001';
const ID = {
  laptop: '01JB000000000000000MCH0001',
  ses1: '01JB000000000000000SES0001',
  ses3: '01JB000000000000000SES0003',
  ses4: '01JB000000000000000SES0004',
} as const;

const consolePath = (session?: string) => `/w/${WS}/console${session === undefined ? '' : `/${session}`}`;
const atPath = (path: string) => new RegExp(`${path.replace(/[?]/g, '\\?')}$`);

const heading = (page: Page, name: string | RegExp) => page.getByRole('heading', { level: 1, name });
const sessionTitle = (page: Page, name: string) => page.getByRole('heading', { level: 2, name });
const sidebar = (page: Page) => page.getByRole('complementary', { name: 'Sidebar' });
const list = (page: Page) => page.getByRole('listbox', { name: 'Sessions' });
const filters = (page: Page) => page.getByRole('group', { name: 'Session filters' });
const transcript = (page: Page) => page.getByRole('group', { name: 'Transcript' });
const composer = (page: Page) => page.getByRole('textbox', { name: 'Message to the agent' });
const row = (page: Page, id: string) => page.locator(`[data-session="${id}"]`);

// The dev server compiles the app on its first page load, which can take longer than a test.
test.beforeAll(async ({ browser }) => {
  test.setTimeout(180_000);
  const page = await browser.newPage();
  await page.goto('/', { timeout: 170_000 });
  await expect(heading(page, 'Home')).toBeVisible({ timeout: 60_000 });
  await page.close();
});

async function axeViolations(page: Page): Promise<string[]> {
  const result = await new AxeBuilder({ page }).analyze();
  expect(result.passes.length).toBeGreaterThan(20);
  return result.violations.map((v) => `${v.id}: ${v.nodes.map((n) => n.target.join(' ')).join(', ')}`);
}

test('opens from the sidebar and from the palette', async ({ page }) => {
  await page.goto('/');
  await expect(page).toHaveURL(/\/home$/);
  await sidebar(page).getByRole('link', { name: /^Agent console/ }).click();
  await expect(page).toHaveURL(atPath(consolePath()));
  await expect(heading(page, 'Agent console')).toBeVisible();
  await expect(list(page).getByRole('option')).toHaveCount(6);
  await expect(page.getByText('Choose a session')).toBeVisible();

  // From the Projects layout, a palette command opens the console already filtered.
  await page.getByRole('radio', { name: 'Projects' }).click();
  await expect(heading(page, 'Home')).toBeVisible();
  await page.keyboard.press('Control+KeyK');
  await expect(page.getByRole('combobox', { name: 'Search' })).toBeFocused();
  await page.keyboard.type('Show sessions waiting');
  await expect(page.getByRole('option').first()).toContainText('Show sessions waiting for input');
  await page.keyboard.press('Enter');
  await expect(page).toHaveURL(atPath(`${consolePath()}?state=waiting`));
  await expect(list(page).getByRole('option')).toHaveCount(1);
  await expect(list(page)).toBeFocused();
});

test('filters by machine and state; the URL keeps them across a reload', async ({ page }) => {
  await page.goto(consolePath());
  await expect(list(page).getByRole('option')).toHaveCount(6);
  await filters(page).getByRole('checkbox', { name: /^This laptop/ }).check();
  await expect(page).toHaveURL(atPath(`${consolePath()}?machine=${ID.laptop}`));
  await filters(page).getByRole('checkbox', { name: /^Waiting/ }).check();
  await expect(page).toHaveURL(atPath(`${consolePath()}?machine=${ID.laptop}&state=waiting`));
  await expect(list(page).getByRole('option')).toHaveText([/Review parser benchmarks/]);
  await expect(page.getByTestId('session-count')).toHaveText('1 of 6 sessions');

  await page.reload();
  await expect(filters(page).getByRole('checkbox', { name: /^This laptop/ })).toBeChecked();
  await expect(filters(page).getByRole('checkbox', { name: /^Waiting/ })).toBeChecked();
  await expect(list(page).getByRole('option')).toHaveText([/Review parser benchmarks/]);

  await filters(page).getByRole('button', { name: 'Clear' }).click();
  await expect(page).toHaveURL(atPath(consolePath()));
  await expect(list(page).getByRole('option')).toHaveCount(6);
});

test("shows SES0001's whole transcript", async ({ page }, info) => {
  await page.goto(consolePath());
  await row(page, ID.ses1).click();
  await expect(page).toHaveURL(atPath(consolePath(ID.ses1)));
  await expect(sessionTitle(page, 'Draft method section')).toBeVisible();
  await expect(row(page, ID.ses1)).toHaveAttribute('aria-selected', 'true');
  // It opens at the newest item...
  await expect(transcript(page).getByText('Go ahead with §3.2 and compare both schedules.')).toBeVisible();
  await page.screenshot({ path: info.outputPath('session-light.png') });
  // ...and scrolls back to the first.
  await transcript(page).evaluate((element) => element.scrollTo({ top: 0 }));
  await expect(transcript(page).getByText('Start of the transcript')).toBeVisible();
  await expect(transcript(page).getByText(/^Draft section 3 \(Method\) from notes\/method-outline\.md/)).toBeVisible();
});

test('moves between panes with F6; the arrows choose a session; the composer keeps its keys', async ({ page }) => {
  await page.goto(consolePath(ID.ses4));
  await expect(row(page, ID.ses4)).toHaveAttribute('aria-selected', 'true');
  await expect(composer(page)).toBeEnabled();

  await filters(page).getByRole('checkbox').first().focus();
  await page.keyboard.press('F6');
  await expect(list(page)).toBeFocused();
  await page.keyboard.press('ArrowUp');
  await expect(page).toHaveURL(atPath(consolePath(ID.ses3)));
  await expect(sessionTitle(page, 'Review parser benchmarks')).toBeVisible();
  await expect(list(page)).toBeFocused();
  await page.keyboard.press('ArrowDown');
  await expect(page).toHaveURL(atPath(consolePath(ID.ses4)));
  await page.keyboard.press('Enter');
  await expect(composer(page)).toBeFocused();

  // In the composer, Ctrl B and Ctrl J are left to the text field; Ctrl K still opens the palette.
  await page.keyboard.press('Control+KeyB');
  await page.keyboard.press('Control+KeyJ');
  await expect(sidebar(page)).toHaveAttribute('data-collapsed', 'false');
  await expect(page.getByRole('complementary', { name: 'Orchestrator' })).toHaveCount(0);
  await expect(composer(page)).toBeFocused();
  await page.keyboard.press('Control+KeyK');
  await expect(page.getByRole('combobox', { name: 'Search' })).toBeFocused();
  await page.keyboard.press('Escape');
  await expect(composer(page)).toBeFocused();

  await page.keyboard.press('Shift+F6');
  await expect(transcript(page)).toBeFocused();
  await page.keyboard.press('PageUp');
  await page.keyboard.press('F6');
  await expect(composer(page)).toBeFocused();
  await page.keyboard.press('F6');
  await expect(filters(page).getByRole('checkbox').first()).toBeFocused();
});

test('sends a prompt and the reply arrives live', async ({ page }) => {
  await page.goto(consolePath(ID.ses4));
  await expect(sessionTitle(page, 'Codex rollout parser')).toBeVisible();
  await expect(row(page, ID.ses4)).toHaveAttribute('data-state', 'idle');
  await composer(page).fill('Summarise the parser change');
  await composer(page).press('Enter');
  await expect(composer(page)).toHaveValue('');
  await expect(transcript(page).getByText('Summarise the parser change', { exact: true })).toBeVisible();
  await expect(row(page, ID.ses4)).toHaveAttribute('data-state', 'working');
  await expect(transcript(page).getByText(/^Mock reply to "Summarise the parser change"/)).toBeVisible();
  // The turn ends, and the agent waits for the next prompt.
  await expect(row(page, ID.ses4)).toHaveAttribute('data-state', 'waiting');
  await expect(composer(page)).toBeEnabled();
});

test('answers the question in SES0003', async ({ page }, info) => {
  await page.goto(consolePath(ID.ses3));
  const card = page.getByRole('region', { name: /Merge the benchmark change into parsers\?/ });
  await expect(card).toBeVisible();
  await page.screenshot({ path: info.outputPath('question.png') });
  await card.getByRole('button', { name: 'Merge it' }).click();
  await expect(card.getByTestId('answer')).toHaveText('Answered: Merge it');
  // The agent carries on, live, and the question can no longer be answered.
  await expect(transcript(page).getByText('Thanks. Carrying on with that.')).toBeVisible();
  await expect(card.getByRole('button', { name: 'Merge it' })).toBeDisabled();
  await expect(card.getByRole('textbox')).toHaveCount(0);
});

test('follows the task link into Projects', async ({ page }) => {
  await page.goto(consolePath(ID.ses1));
  const linked = page.getByRole('navigation', { name: 'Linked work' });
  const task = linked.getByRole('link', { name: /^PAP-1 · / });
  await expect(task).toHaveAttribute('href', `/w/${WS}/tasks/PAP-1`);
  await expect(linked.getByRole('link', { name: 'Submission' })).toHaveAttribute(
    'href',
    /\/projects\/[^/]+\/workstreams\/[^/]+$/,
  );
  await task.click();
  await expect(page).toHaveURL(/\/tasks\/PAP-1$/);
  await expect(heading(page, /^PAP-1/)).toBeVisible();
  await expect(page.getByRole('radio', { name: 'Projects' })).toHaveAttribute('aria-checked', 'true');
  await page.goBack();
  await expect(page).toHaveURL(atPath(consolePath(ID.ses1)));
  await expect(sessionTitle(page, 'Draft method section')).toBeVisible();
});

test('a narrow window shows one pane at a time, with a way back', async ({ page }, info) => {
  await page.setViewportSize({ width: 700, height: 800 });
  await page.goto(consolePath());
  await expect(page.locator('[data-console-layout]')).toHaveAttribute('data-console-layout', 'narrow');
  await expect(list(page)).toBeVisible();
  await expect(filters(page)).toHaveCount(0);

  await page.getByRole('button', { name: 'Filters' }).click();
  await expect(filters(page)).toBeVisible();
  await expect(list(page)).toHaveCount(0);
  await page.getByRole('button', { name: 'Sessions' }).click();
  await expect(list(page)).toBeVisible();

  await row(page, ID.ses1).click();
  await expect(sessionTitle(page, 'Draft method section')).toBeVisible();
  await expect(list(page)).toHaveCount(0);
  await page.screenshot({ path: info.outputPath('narrow-session.png') });
  await page.getByRole('button', { name: 'Sessions' }).click();
  await expect(page).toHaveURL(atPath(consolePath()));
  await expect(list(page)).toBeVisible();

  // Wide again, the three panes come back.
  await page.setViewportSize({ width: 1280, height: 800 });
  await expect(page.locator('[data-console-layout]')).toHaveAttribute('data-console-layout', 'wide');
  await expect(filters(page)).toBeVisible();
});

for (const theme of ['light', 'dark'] as const) {
  for (const layout of ['wide', 'narrow'] as const) {
    test(`axe finds no violations: ${layout}, ${theme}`, async ({ page }, info) => {
      await page.emulateMedia({ colorScheme: theme, reducedMotion: 'reduce' });
      if (layout === 'narrow') await page.setViewportSize({ width: 700, height: 800 });
      await page.goto(`${consolePath(ID.ses1)}?state=working`);
      await page.getByRole('radio', { name: theme === 'light' ? 'Light' : 'Dark' }).click();
      await expect(page.locator('html')).toHaveAttribute('data-theme', theme);
      await expect(page.locator('[data-console-layout]')).toHaveAttribute('data-console-layout', layout);

      // The session, with its plan, a tool's input and output, and an edit's diff open.
      await expect(transcript(page).getByText('Go ahead with §3.2 and compare both schedules.')).toBeVisible();
      await page.getByRole('button', { name: /^Plan/ }).click();
      await transcript(page).getByRole('button', { name: /^Edited method\.tex/ }).last().click();
      await transcript(page).getByRole('button', { name: /^Bash/ }).last().click();
      await expect(transcript(page).locator('[aria-label^="Changes to"]').first()).toBeVisible();
      await page.screenshot({ path: info.outputPath(`session-${layout}-${theme}.png`) });
      expect(await axeViolations(page)).toEqual([]);

      // The list with a filter on (in a narrow window, the list's own pane).
      if (layout === 'narrow') await page.getByRole('button', { name: 'Sessions' }).click();
      await expect(list(page).getByRole('option')).toHaveCount(2);
      if (layout === 'narrow') {
        await page.getByRole('button', { name: /^Filters/ }).click();
        await expect(filters(page)).toBeVisible();
      }
      await page.screenshot({ path: info.outputPath(`list-${layout}-${theme}.png`) });
      expect(await axeViolations(page)).toEqual([]);
    });
  }
}
