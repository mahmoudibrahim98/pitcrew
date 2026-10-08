import { expect, test, type APIRequestContext, type Locator, type Page } from '@playwright/test';
import { expectNoAxeViolations } from '../../../../e2e/axe.ts';
import { HUB_AUTH } from '../../../../e2e/helpers.ts';

// The workbench's acceptance, in the running app against the mock hub: open, split, drag,
// reload restores; a file with colouring and a PDF; the keys; axe in both themes. Runs under this
// folder's config and, through e2e/workbench.spec.ts, under the UI's own (CI's) config, so it
// finds the hub from the app's own requests rather than from a port it assumes.

const WS = '01JB000000000000000WSP0001';
const SUBMISSION = '01JB000000000000000WST0001';
const SES1 = '01JB000000000000000SES0001';
const SES4 = '01JB000000000000000SES0004';
const consolePath = (session?: string) => `/w/${WS}/console${session === undefined ? '' : `/${session}`}`;

const TS_FILE = 'src/workbench-demo.ts';
const PDF_FILE = 'workbench-demo.pdf';

/** A one-page PDF saying "Workbench PDF", built with a correct cross-reference table. */
function tinyPdf(): string {
  const text = 'BT /F1 24 Tf 40 70 Td (Workbench PDF) Tj ET';
  const objects = [
    '<< /Type /Catalog /Pages 2 0 R >>',
    '<< /Type /Pages /Kids [3 0 R] /Count 1 >>',
    '<< /Type /Page /Parent 2 0 R /MediaBox [0 0 300 144] /Contents 4 0 R /Resources << /Font << /F1 5 0 R >> >> >>',
    `<< /Length ${text.length} >>\nstream\n${text}\nendstream`,
    '<< /Type /Font /Subtype /Type1 /BaseFont /Helvetica >>',
  ];
  let body = '%PDF-1.4\n';
  const offsets: number[] = [];
  objects.forEach((object, i) => {
    offsets.push(body.length);
    body += `${i + 1} 0 obj\n${object}\nendobj\n`;
  });
  const xref = body.length;
  body += `xref\n0 ${objects.length + 1}\n0000000000 65535 f \n`;
  for (const offset of offsets) body += `${String(offset).padStart(10, '0')} 00000 n \n`;
  body += `trailer\n<< /Size ${objects.length + 1} /Root 1 0 R >>\nstartxref\n${xref}\n%%EOF\n`;
  return body;
}

/** The hub the app talks to: whichever config started it, on whatever port. */
async function openConsole(page: Page, path: string): Promise<string> {
  const first = page.waitForRequest((request) => new URL(request.url()).pathname.startsWith('/v1/'));
  await page.goto(path);
  return new URL((await first).url()).origin;
}

/** Writes a file into the Submission workstream's folder, over whatever an earlier run left. */
async function seed(request: APIRequestContext, hub: string, path: string, content: string): Promise<void> {
  const url = `${hub}/v1/workstreams/${SUBMISSION}/files/content?loc=0&path=${encodeURIComponent(path)}`;
  const current = await request.get(url, { headers: HUB_AUTH });
  const revision = current.ok() ? ((await current.json()) as { revision: string }).revision : null;
  const written = await request.put(url, { headers: HUB_AUTH, data: { revision, encoding: 'utf8', content } });
  expect(written.ok()).toBe(true);
}

const pane = (page: Page, n: number) => page.getByRole('region', { name: `Pane ${n}`, exact: true });
const tabs = (page: Page, n: number) => page.getByRole('tablist', { name: `Tabs in pane ${n}` }).getByRole('tab');
const details = (page: Page) => page.getByRole('complementary', { name: 'Details' });

/** Each pane's tab names, as the accessibility tree reads them. */
async function picture(page: Page): Promise<string[][]> {
  const lists = page.getByRole('tablist');
  const out: string[][] = [];
  for (let i = 0; i < (await lists.count()); i += 1) {
    out.push(await lists.nth(i).getByRole('tab').evaluateAll((all) => all.map((t) => (t.textContent ?? '').trim())));
  }
  return out;
}

/** Drags a tab with the pointer, as a person would, to `target` at `x`, `y` (fractions of it). */
async function drag(tab: Locator, target: Locator, x: number, y: number): Promise<void> {
  const box = await target.boundingBox();
  if (box === null) throw new Error('no target');
  await tab.dragTo(target, { targetPosition: { x: box.width * x, y: box.height * y } });
}

// The dev server compiles the app on its first page load, which can take longer than a test.
test.beforeAll(async ({ browser }) => {
  test.setTimeout(180_000);
  const page = await browser.newPage();
  await page.goto('/', { timeout: 170_000 });
  await expect(page.getByRole('heading', { level: 1, name: 'Home' })).toBeVisible({ timeout: 60_000 });
  await page.close();
});

test('opens, splits, drags, and a reload restores the layout', async ({ page, request }, info) => {
  test.setTimeout(150_000);
  // Room for three panes and the details beside the list.
  await page.setViewportSize({ width: 1600, height: 900 });
  await page.emulateMedia({ reducedMotion: 'reduce' });
  const hub = await openConsole(page, consolePath(SES1));
  await page.getByRole('button', { name: 'Filters', pressed: true }).click();
  await seed(
    request,
    hub,
    TS_FILE,
    '// Synthetic sample for the workbench test.\nexport function greet(name: string): string {\n  return `hi ${name}`;\n}\nconst answer = 42;\n',
  );
  await seed(request, hub, PDF_FILE, tinyPdf());

  // The session chosen in the URL opens as the pane's preview tab; a double-click keeps it.
  await expect(tabs(page, 1)).toHaveCount(1);
  await expect(tabs(page, 1).first()).toHaveAccessibleName(/^Draft method section.*\(preview\)/);
  await tabs(page, 1).first().dblclick();
  await expect(tabs(page, 1).first()).not.toHaveAccessibleName(/preview/);

  // Split right: the same session beside it, switched to its terminal.
  await pane(page, 1).getByRole('button', { name: 'Split right' }).click();
  await expect(pane(page, 2)).toBeVisible();
  await page.setViewportSize({ width: 1100, height: 900 });
  await expect(page.locator('[data-split][data-stacked="true"]')).toHaveCount(1);
  for (const number of [1, 2]) {
    expect((await pane(page, number).boundingBox())?.width).toBeGreaterThanOrEqual(320);
  }
  await page.setViewportSize({ width: 1600, height: 900 });
  await expect(page.locator('[data-split][data-stacked="true"]')).toHaveCount(0);
  await expect(tabs(page, 1)).toHaveCount(1);
  await expect(tabs(page, 2)).toHaveCount(1);
  await pane(page, 2).getByRole('radio', { name: 'Terminal' }).click();
  await expect(pane(page, 2).getByRole('group', { name: 'Terminal' })).toBeVisible();
  await expect(pane(page, 1).getByRole('group', { name: 'Transcript' })).toBeVisible();
  await expect(page).toHaveURL(new RegExp(`${consolePath(SES1)}\\?view=terminal$`));

  // The details: what the session is, and its workstream's files.
  await pane(page, 2).getByRole('button', { name: 'Details' }).click();
  await expect(details(page)).toBeVisible();
  for (const [term, value] of [
    ['Task', /^PAP-1 · /],
    ['Workstream', 'Submission'],
    ['Machine', 'This laptop'],
    ['Model', "The CLI's default (Writer)"],
    ['Account', 'Not reported'],
  ] as const) {
    await expect(details(page).locator('dt', { hasText: term }).locator('xpath=following-sibling::dd[1]')).toHaveText(value);
  }
  const tree = details(page).getByRole('navigation', { name: 'Folder tree' });
  await tree.getByRole('button', { name: '▸ src' }).click();
  await tree.getByRole('button', { name: 'workbench-demo.ts' }).click();
  await expect(tabs(page, 2)).toHaveCount(2);
  const text = pane(page, 2).getByLabel('File text');
  await expect(text).toHaveAttribute('data-language', 'javascript');
  await expect(text.locator('[data-token="keyword"]', { hasText: 'function' })).toBeVisible();
  await expect(text.locator('[data-token="comment"]')).toHaveText('// Synthetic sample for the workbench test.');

  // Drag the file's tab into pane 1's tab strip, at its start.
  const fileTab = tabs(page, 2).filter({ hasText: 'workbench-demo.ts' });
  await drag(fileTab, page.getByRole('tablist', { name: 'Tabs in pane 1' }), 0.02, 0.5);
  await expect.poll(() => picture(page)).toEqual([['workbench-demo.ts', 'Draft method section'], ['Draft method section· Terminal']]);

  // The PDF, opened from the tree into pane 1, drawn by pdf.js.
  await tree.getByRole('button', { name: PDF_FILE }).click();
  const firstPage = pane(page, 1).getByRole('img', { name: 'Page 1 of 1' });
  await expect(firstPage).toHaveAttribute('data-page-state', 'drawn', { timeout: 20_000 });
  await expect(pane(page, 1).getByText('1 page', { exact: true })).toBeVisible();

  // Drag the PDF's tab to pane 2's bottom edge: a third pane under the terminal.
  const pdfTab = tabs(page, 1).filter({ hasText: PDF_FILE });
  await drag(pdfTab, pane(page, 2).getByRole('tabpanel'), 0.5, 0.95);
  await expect(pane(page, 3)).toBeVisible();
  await expect.poll(() => picture(page)).toEqual([
    ['workbench-demo.ts', 'Draft method section'],
    ['Draft method section· Terminal'],
    [PDF_FILE],
  ]);
  await expect(pane(page, 3).getByRole('img', { name: 'Page 1 of 1' })).toHaveAttribute('data-page-state', 'drawn', {
    timeout: 20_000,
  });
  await page.screenshot({ path: info.outputPath('workbench.png') });
  await expectNoAxeViolations(page, 'the workbench with a chat, a terminal, a PDF and the details');

  // A reload: the same panes, tabs and tab on screen, from this browser's storage.
  const before = await picture(page);
  const url = page.url();
  await page.reload();
  await expect(pane(page, 3)).toBeVisible();
  expect(await picture(page)).toEqual(before);
  await expect(page).toHaveURL(url);
  await expect(details(page)).toBeVisible();
  await expect(pane(page, 3).getByRole('img', { name: 'Page 1 of 1' })).toHaveAttribute('data-page-state', 'drawn', {
    timeout: 20_000,
  });
  await expect(pane(page, 2).getByRole('group', { name: 'Terminal' })).toBeVisible();
  await expect(tabs(page, 1).filter({ hasText: 'Draft method section' })).toHaveAttribute('aria-selected', 'true');
});

test('a dragged divider keeps each pane at its minimum width; a stacked split has no divider', async ({ page }) => {
  await page.setViewportSize({ width: 1600, height: 900 });
  await openConsole(page, consolePath(SES1));
  await expect(tabs(page, 1)).toHaveCount(1);
  await pane(page, 1).getByRole('button', { name: 'Split right' }).click();
  await expect(pane(page, 2)).toBeVisible();
  const stacked = page.locator('[data-split][data-stacked="true"]');
  await expect(stacked).toHaveCount(0);

  // Dragged far past pane 1's minimum: the split stays side by side, pane 1 at its minimum.
  const divider = page.getByRole('separator', { name: 'Resize the panes side by side' });
  const box = await divider.boundingBox();
  if (box === null) throw new Error('no divider');
  await page.mouse.move(box.x + box.width / 2, box.y + box.height / 2);
  await page.mouse.down();
  await page.mouse.move(box.x - 900, box.y + box.height / 2, { steps: 12 });
  await page.mouse.up();
  await expect(stacked).toHaveCount(0);
  const width = (await pane(page, 1).boundingBox())?.width ?? 0;
  expect(width).toBeGreaterThanOrEqual(319);
  expect(width).toBeLessThan(330);
  const position = (await divider.getAttribute('aria-valuenow')) ?? '';

  // Too narrow for both: stacked, with nothing to drag; wide again, the saved split is back.
  await page.setViewportSize({ width: 1100, height: 900 });
  await expect(stacked).toHaveCount(1);
  await expect(page.locator('[data-splitter]')).toHaveCount(0);
  await page.setViewportSize({ width: 1600, height: 900 });
  await expect(stacked).toHaveCount(0);
  await expect(divider).toHaveAttribute('aria-valuenow', position);
});

test('the keys and the palette switch, move and close tabs', async ({ page }) => {
  test.setTimeout(90_000);
  await openConsole(page, consolePath(SES1));
  await expect(tabs(page, 1)).toHaveCount(1);
  // Choosing another session from the list replaces the preview; its row menu opens a kept tab.
  await page.locator(`[data-session="${SES4}"]`).click({ button: 'right' });
  await page.getByRole('menuitem', { name: 'Open in a new tab' }).click();
  await expect.poll(() => picture(page)).toEqual([['Draft method section(preview)', 'Codex rollout parser']]);
  await expect(page).toHaveURL(new RegExp(`${consolePath(SES4)}$`));

  // The tab strip: arrows switch, Shift with an arrow moves.
  const codex = tabs(page, 1).filter({ hasText: 'Codex rollout parser' });
  await expect(codex).toBeFocused();
  await page.keyboard.press('ArrowLeft');
  await expect(tabs(page, 1).first()).toBeFocused();
  await expect(page).toHaveURL(new RegExp(`${consolePath(SES1)}$`));
  // Moving a preview tab keeps it.
  await page.keyboard.press('Shift+ArrowRight');
  await expect.poll(() => picture(page)).toEqual([['Codex rollout parser', 'Draft method section']]);

  // Alt PageUp and PageDown switch from anywhere; Alt \ splits; Alt Shift PageUp moves back.
  await page.keyboard.press('Alt+PageUp');
  await expect(codex).toHaveAttribute('aria-selected', 'true');
  await page.keyboard.press('Alt+PageDown');
  await expect(tabs(page, 1).nth(1)).toHaveAttribute('aria-selected', 'true');
  await page.keyboard.press('Alt+Backslash');
  await expect(pane(page, 2)).toBeVisible();
  await page.keyboard.press('Alt+Shift+PageUp');
  await expect(pane(page, 2)).toHaveCount(0);

  // The palette has an entry for each.
  await page.keyboard.press('Control+KeyK');
  await expect(page.getByRole('combobox', { name: 'Search' })).toBeFocused();
  await page.keyboard.type('Close the tab');
  await expect(page.getByRole('option').first()).toContainText('Close the tab');
  await page.keyboard.press('Enter');
  await expect.poll(() => picture(page)).toEqual([['Codex rollout parser']]);
  await expect(tabs(page, 1).first()).toBeFocused();
  // Delete closes the focused tab; the console then asks for a session again.
  await page.keyboard.press('Delete');
  await expect(page.getByText('Choose a session')).toBeVisible();
  await expect(page).toHaveURL(new RegExp(`${consolePath()}$`));
});

test('a corrupt stored layout starts afresh', async ({ page }) => {
  await page.addInitScript((key) => {
    window.localStorage.setItem(key, '{"version":1,"layout":{"root":{"type":"split","children":[]}}');
  }, `pitcrew.workbench.${WS}`);
  await openConsole(page, consolePath());
  await expect(page.getByText('Choose a session')).toBeVisible();
  await expect(tabs(page, 1)).toHaveCount(0);
  await page.locator(`[data-session="${SES4}"]`).click();
  await expect(tabs(page, 1)).toHaveCount(1);
});
