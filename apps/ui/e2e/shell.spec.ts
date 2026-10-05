import AxeBuilder from '@axe-core/playwright';
import { expect, test, type Page } from '@playwright/test';
import { HUB_AUTH, HUB_URL } from './helpers';

const PAPER = '01JB000000000000000PRJ0001';

const sidebar = (page: Page) => page.getByRole('complementary', { name: 'Sidebar' });
const tree = (page: Page) => page.getByRole('tree', { name: 'Projects' });
const heading = (page: Page, name: string | RegExp) => page.getByRole('heading', { level: 1, name });
const orchestrator = (page: Page) => page.getByRole('complementary', { name: 'Orchestrator' });

async function openShell(page: Page) {
  await page.goto('/');
  await expect(page).toHaveURL(/\/w\/[^/]+\/home$/);
  await expect(heading(page, 'Home')).toBeVisible();
  // Counts render once the stream has synced and the lists have loaded.
  await expect(page.getByTestId('count-my-tasks')).toBeVisible();
}

/** The leading number of a count badge ("3 open tasks" → 3). */
async function count(page: Page, testId: string): Promise<number> {
  const text = (await page.getByTestId(testId).textContent()) ?? '';
  const match = /^\d+/.exec(text.trim());
  if (match === null) throw new Error(`${testId} shows "${text}"`);
  return Number(match[0]);
}

test('navigates Home → a project → a workstream', async ({ page }, info) => {
  await openShell(page);
  await sidebar(page).getByRole('link', { name: 'Inbox', exact: false }).click();
  await expect(heading(page, 'Inbox')).toBeVisible();
  await sidebar(page).getByRole('link', { name: 'Home', exact: true }).click();
  await expect(heading(page, 'Home')).toBeVisible();

  await tree(page).getByRole('treeitem', { name: /^Paper · Diffusion study/ }).click();
  await expect(heading(page, 'Paper · Diffusion study')).toBeVisible();
  await expect(page).toHaveURL(new RegExp(`/projects/${PAPER}$`));

  const seedRuns = tree(page).getByRole('treeitem', { name: /Seed runs/ });
  await seedRuns.click();
  await expect(heading(page, 'Seed runs')).toBeVisible();
  await expect(page).toHaveURL(/\/projects\/[^/]+\/workstreams\/[^/]+$/);
  await expect(seedRuns).toHaveAttribute('aria-current', 'page');
  await expect(page.getByRole('navigation', { name: 'Breadcrumb' })).toContainText(
    'Paper · Diffusion study/Seed runs',
  );
  await page.screenshot({ path: info.outputPath('workstream-light.png') });
});

test('the projects tree works from the keyboard', async ({ page }) => {
  await openShell(page);
  await sidebar(page).getByRole('link', { name: /^Agent console/ }).focus();
  await page.keyboard.press('Tab');
  const paper = tree(page).getByRole('treeitem', { name: /^Paper · Diffusion study/ });
  await expect(paper).toBeFocused();
  await expect(paper).toHaveAttribute('aria-expanded', 'false');

  await page.keyboard.press('ArrowRight');
  await expect(paper).toHaveAttribute('aria-expanded', 'true');
  await page.keyboard.press('ArrowRight');
  await expect(tree(page).getByRole('treeitem', { name: /Submission/ })).toBeFocused();
  await page.keyboard.press('ArrowDown');
  const seedRuns = tree(page).getByRole('treeitem', { name: /Seed runs/ });
  await expect(seedRuns).toBeFocused();
  await page.keyboard.press('Enter');
  await expect(heading(page, 'Seed runs')).toBeVisible();

  await page.keyboard.press('ArrowLeft');
  await expect(paper).toBeFocused();
  await page.keyboard.press('ArrowLeft');
  await expect(paper).toHaveAttribute('aria-expanded', 'false');
  await page.keyboard.press('End');
  await expect(tree(page).getByRole('treeitem', { name: /^Tooling/ })).toBeFocused();
  await page.keyboard.press('p');
  await expect(paper).toBeFocused();
});

test('Ctrl . switches layouts, and each layout reopens where it was left', async ({ page }) => {
  await openShell(page);
  await tree(page).getByRole('treeitem', { name: /^Tooling/ }).click();
  await expect(heading(page, 'Tooling')).toBeVisible();

  await page.keyboard.press('Control+Period');
  await expect(page).toHaveURL(/\/console$/);
  await expect(heading(page, 'Agent console')).toBeVisible();
  await expect(page.getByRole('radio', { name: 'Agent console' })).toHaveAttribute('aria-checked', 'true');
  await expect(tree(page)).toHaveCount(0);

  await page.keyboard.press('Control+Period');
  await expect(heading(page, 'Tooling')).toBeVisible();
  await expect(page.getByRole('radio', { name: 'Projects' })).toHaveAttribute('aria-checked', 'true');

  // The choice persists per workspace: the workspace reopens in the console.
  await page.keyboard.press('Control+Period');
  await expect(page).toHaveURL(/\/console$/);
  const workspace = /\/w\/([^/]+)\//.exec(page.url())?.[1] ?? '';
  await page.goto(`/w/${workspace}`);
  await expect(page).toHaveURL(/\/console$/);

  // The toggle does the same by pointer.
  await page.getByRole('radio', { name: 'Projects' }).click();
  await expect(heading(page, 'Tooling')).toBeVisible();
});

test('Ctrl K opens the palette and jumps to PAP-4', async ({ page }, info) => {
  await openShell(page);
  await page.keyboard.press('Control+KeyK');
  const input = page.getByRole('combobox', { name: 'Search' });
  await expect(input).toBeFocused();
  await page.keyboard.type('PAP-4');
  const first = page.getByRole('option').first();
  await expect(first).toContainText('PAP-4');
  await expect(first).toContainText('Run seeds');
  await expect(first).toHaveAttribute('aria-selected', 'true');
  await page.screenshot({ path: info.outputPath('palette-light.png') });
  await page.keyboard.press('Enter');

  await expect(page).toHaveURL(/\/tasks\/PAP-4$/);
  await expect(heading(page, /^PAP-4 · Run seeds/)).toBeVisible();
  await expect(page.getByRole('dialog')).toHaveCount(0);
  await expect(page.locator('#main')).toBeFocused();

  // Arrow keys move through the results; Esc closes and gives focus back.
  const search = page.getByRole('button', { name: /^Search/ });
  await search.focus();
  await page.keyboard.press('Control+KeyK');
  await expect(input).toBeFocused();
  await page.keyboard.type('seed runs');
  await expect(page.getByRole('option').first()).toContainText('Seed runs');
  await page.keyboard.press('ArrowDown');
  await expect(page.getByRole('option').nth(1)).toHaveAttribute('aria-selected', 'true');

  // In the palette's text field, the other shell shortcuts leave the field alone; Ctrl K closes.
  // (The modal hides the page from the accessibility tree, so these look it up by markup.)
  await page.keyboard.press('Control+KeyB');
  await page.keyboard.press('Control+KeyJ');
  await page.keyboard.press('Control+Period');
  await expect(page.locator('aside[data-collapsed]')).toHaveAttribute('data-collapsed', 'false');
  await expect(page.locator('aside[aria-label="Orchestrator"]')).toHaveCount(0);
  await expect(page).toHaveURL(/\/tasks\/PAP-4$/);
  await page.keyboard.press('Control+KeyK');
  await expect(page.getByRole('dialog')).toHaveCount(0);
  await expect(search).toBeFocused();

  await search.focus();
  await page.keyboard.press('Control+KeyK');
  await page.keyboard.press('Escape');
  await expect(page.getByRole('dialog')).toHaveCount(0);
  await expect(search).toBeFocused();
});

test('Ctrl J opens the Orchestrator panel; it resizes and stays open after a reload', async ({ page }) => {
  await openShell(page);
  await expect(orchestrator(page)).toHaveCount(0);
  await page.keyboard.press('Control+KeyJ');
  await expect(orchestrator(page)).toBeVisible();
  await expect(orchestrator(page).getByText('Ask about your work')).toBeVisible();

  const edge = page.getByRole('separator', { name: 'Resize Orchestrator' });
  const width = Number(await edge.getAttribute('aria-valuenow'));
  await edge.focus();
  await page.keyboard.press('ArrowLeft');
  await expect(edge).toHaveAttribute('aria-valuenow', String(width + 16));

  await page.reload();
  await expect(orchestrator(page)).toBeVisible();
  await expect(edge).toHaveAttribute('aria-valuenow', String(width + 16));
  await page.keyboard.press('Control+KeyJ');
  await expect(orchestrator(page)).toHaveCount(0);
});

test('a task moved through the API updates the sidebar counts without a reload', async ({ page, request }) => {
  await openShell(page);
  await page.evaluate(() => {
    (window as unknown as { marker: boolean }).marker = true;
  });
  const myTasks = await count(page, 'count-my-tasks');
  const paper = await count(page, `open-${PAPER}`);

  // PAP-7 is assigned to the workspace's person. Reopening it adds an open task to My tasks and to
  // its project; closing it again takes one away.
  const task = (await (await request.get(`${HUB_URL}/v1/tasks/PAP-7`, { headers: HUB_AUTH })).json()) as {
    status: string;
  };
  const reopen = task.status === 'done';
  const moved = await request.post(`${HUB_URL}/v1/tasks/PAP-7/move`, {
    headers: HUB_AUTH,
    data: { to: reopen ? 'todo' : 'done' },
  });
  expect(moved.ok()).toBe(true);

  const step = reopen ? 1 : -1;
  await expect.poll(() => count(page, 'count-my-tasks')).toBe(myTasks + step);
  await expect.poll(() => count(page, `open-${PAPER}`)).toBe(paper + step);
  expect(await page.evaluate(() => (window as unknown as { marker?: boolean }).marker)).toBe(true);
});

test('"+ New" opens creation dialogs from the keyboard', async ({ page }, info) => {
  await openShell(page);
  const trigger = page.getByRole('button', { name: 'New', exact: true });
  await trigger.focus();
  await page.keyboard.press('Enter');
  const items = page.getByRole('menuitem');
  await expect(items).toHaveText(['Task', 'Agent', 'Project', 'Team', 'Session']);
  await page.screenshot({ path: info.outputPath('new-menu.png') });
  await page.keyboard.press('ArrowDown');
  await page.keyboard.press('Enter');
  const dialog = page.getByRole('dialog', { name: 'New agent' });
  await expect(dialog).toBeVisible();
  await expect(dialog.getByLabel('Name', { exact: true })).toBeFocused();
  await page.screenshot({ path: info.outputPath('new-dialog.png') });
  await page.keyboard.press('Escape');
  await expect(dialog).toHaveCount(0);
  await expect(trigger).toBeFocused();

  // Opened from the palette, a dialog gives focus back to what had it before the palette.
  const home = sidebar(page).getByRole('link', { name: 'Home', exact: true });
  await home.focus();
  await page.keyboard.press('Control+KeyK');
  await expect(page.getByRole('combobox', { name: 'Search' })).toBeFocused();
  await page.keyboard.type('new team');
  await expect(page.getByRole('option').first()).toContainText('New team');
  await page.keyboard.press('Enter');
  const team = page.getByRole('dialog', { name: 'New team' });
  await expect(team).toBeVisible();
  await page.keyboard.press('Escape');
  await expect(team).toHaveCount(0);
  await expect(home).toBeFocused();
});

test('Ctrl B collapses the sidebar to a rail of labelled icons', async ({ page }, info) => {
  await openShell(page);
  await page.keyboard.press('Control+KeyB');
  await expect(sidebar(page)).toHaveAttribute('data-collapsed', 'true');
  await page.screenshot({ path: info.outputPath('rail.png') });
  await sidebar(page).getByRole('link', { name: 'Inbox' }).click();
  await expect(heading(page, 'Inbox')).toBeVisible();
  await page.keyboard.press('Control+KeyB');
  await expect(sidebar(page)).toHaveAttribute('data-collapsed', 'false');
});

test('tabbing to a control shows a visible focus ring', async ({ page }) => {
  await openShell(page);
  const outline = () =>
    page.evaluate(() => {
      const el = document.activeElement;
      if (!(el instanceof HTMLElement)) return null;
      const style = getComputedStyle(el);
      return { style: style.outlineStyle, width: style.outlineWidth };
    });

  // Tailwind v4: `outline-none` computes `outline-style: none`, so `focus-visible:outline-2`
  // alone draws nothing. These are the controls the bug was seen on (see design/focus.ts).
  const controls = [
    page.getByRole('button', { name: 'Collapse sidebar' }),
    page.getByRole('button', { name: /^Workspace:/ }),
    sidebar(page).getByRole('link', { name: 'Home', exact: true }),
    page.getByRole('radio', { name: 'Agent console' }),
  ];
  for (const control of controls) {
    await control.focus();
    await expect(control).toBeFocused();
    expect(await outline()).toMatchObject({ style: 'solid' });
  }

  await page.keyboard.press('Control+KeyJ');
  const closeOrchestrator = page.getByRole('button', { name: 'Close the Orchestrator' });
  await closeOrchestrator.focus();
  expect(await outline()).toMatchObject({ style: 'solid' });

  await tree(page).getByRole('treeitem', { name: /^Paper · Diffusion study/ }).focus();
  expect(await outline()).toMatchObject({ style: 'solid' });
});

test('the breadcrumb collapses its middle segments at a narrow window, keeping the full path reachable', async ({
  page,
}) => {
  await openShell(page);
  await tree(page).getByRole('treeitem', { name: /^Paper · Diffusion study/ }).click();
  await tree(page).getByRole('treeitem', { name: /Seed runs/ }).click();
  await expect(heading(page, 'Seed runs')).toBeVisible();

  await page.setViewportSize({ width: 700, height: 800 });
  const nav = page.getByRole('navigation', { name: 'Breadcrumb' });
  const list = nav.locator('ol');
  // The current page's own crumb stays fully readable, not crushed into an ellipsis.
  await expect(list.getByText('Seed runs', { exact: true })).toBeVisible();
  // The collapsed middle segment is a reachable control, not just gone.
  const collapsed = list.getByRole('button', { name: /^Collapsed:/ });
  await expect(collapsed).toBeVisible();
  await expect(collapsed).toHaveAccessibleName(/Paper · Diffusion study/);
  // Assistive tech can still get the whole path, even though it is visually collapsed.
  await expect(list).toHaveAttribute('aria-label', /Paper · Diffusion study.*Seed runs/);

  await page.setViewportSize({ width: 1280, height: 800 });
});

for (const theme of ['light', 'dark'] as const) {
  for (const layout of ['projects', 'console'] as const) {
    test(`axe finds no violations: ${layout} layout, ${theme}`, async ({ page }, info) => {
      await page.emulateMedia({ colorScheme: theme });
      await openShell(page);
      await page.getByRole('radio', { name: theme === 'light' ? 'Light' : 'Dark' }).click();
      await expect(page.locator('html')).toHaveAttribute('data-theme', theme);
      if (layout === 'projects') {
        await tree(page).getByRole('treeitem', { name: /^Paper · Diffusion study/ }).click();
        await tree(page).getByRole('treeitem', { name: /Seed runs/ }).click();
        await expect(heading(page, 'Seed runs')).toBeVisible();
      } else {
        await page.keyboard.press('Control+Period');
        await expect(heading(page, 'Agent console')).toBeVisible();
      }
      await page.keyboard.press('Control+KeyJ');
      await expect(orchestrator(page)).toBeVisible();
      await page.screenshot({ path: info.outputPath(`${layout}-${theme}.png`) });

      const shell = await new AxeBuilder({ page }).analyze();
      expect(shell.passes.length).toBeGreaterThan(20);
      expect(shell.violations.map((v) => `${v.id}: ${v.nodes.map((n) => n.target.join(' ')).join(', ')}`)).toEqual([]);

      await page.keyboard.press('Control+KeyK');
      await expect(page.getByRole('combobox', { name: 'Search' })).toBeFocused();
      await expect(page.getByRole('option').first()).toBeVisible();
      const palette = await new AxeBuilder({ page }).analyze();
      expect(palette.violations.map((v) => `${v.id}: ${v.nodes.map((n) => n.target.join(' ')).join(', ')}`)).toEqual([]);
    });
  }
}
