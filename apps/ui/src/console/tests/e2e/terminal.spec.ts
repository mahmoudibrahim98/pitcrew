import AxeBuilder from '@axe-core/playwright';
import { expect, test, type Page, type WebSocket } from '@playwright/test';

// The terminal's acceptance, in the running app against the mock hub (playwright.config.ts in this
// folder). The mock replays a short canned screen on every connection and echoes keystrokes.
// The screen is read through xterm's screen-reader rows (the WebGL renderer draws on a canvas),
// so the specs turn the screen reader mode on; it is remembered per browser, and each test starts
// with a fresh browser context.

const WS = '01JB000000000000000WSP0001';
const ID = {
  ses1: '01JB000000000000000SES0001',
  ses2: '01JB000000000000000SES0002',
  ses4: '01JB000000000000000SES0004',
  ses5: '01JB000000000000000SES0005',
  ses6: '01JB000000000000000SES0006',
} as const;

const consolePath = (session?: string) => `/w/${WS}/console${session === undefined ? '' : `/${session}`}`;
const terminalPath = (session: string) => `${consolePath(session)}?view=terminal`;
const atPath = (path: string) => new RegExp(`${path.replace(/[?]/g, '\\?')}$`);

const sessionTitle = (page: Page, name: string) => page.getByRole('heading', { level: 2, name });
const sidebar = (page: Page) => page.getByRole('complementary', { name: 'Sidebar' });
const palette = (page: Page) => page.getByRole('combobox', { name: 'Search' });
const frame = (page: Page) => page.getByRole('group', { name: 'Terminal' });
const mode = (page: Page) => page.getByTestId('terminal-mode');
const status = (page: Page) => page.getByTestId('terminal-status');
const terminalOption = (page: Page) => page.getByRole('radio', { name: 'Terminal' });
/** The terminal's rows as xterm exposes them to screen readers. */
const screen = (page: Page) => page.locator('.xterm-accessibility-tree');
const transcript = (page: Page) => page.getByRole('group', { name: 'Transcript' });

test.beforeAll(async ({ browser }) => {
  test.setTimeout(180_000);
  const page = await browser.newPage();
  await page.goto('/', { timeout: 170_000 });
  await expect(page.getByRole('heading', { level: 1, name: 'Home' })).toBeVisible({ timeout: 60_000 });
  await page.close();
});

async function screenReaderMode(page: Page) {
  const toggle = page.getByRole('checkbox', { name: 'Screen reader mode' });
  if (!(await toggle.isChecked())) await toggle.check();
  await expect(screen(page)).toBeAttached();
}

/** Every terminal socket the page opens, with the keystrokes sent on each. */
function watchTerminals(page: Page) {
  const sockets: { url: string; socket: WebSocket; keys: Buffer[] }[] = [];
  page.on('websocket', (socket) => {
    if (!socket.url().includes('/terminal')) return;
    const entry = { url: socket.url(), socket, keys: [] as Buffer[] };
    socket.on('framesent', (frame) => {
      if (typeof frame.payload !== 'string') entry.keys.push(frame.payload);
    });
    sockets.push(entry);
  });
  return {
    sockets,
    keys: () => Buffer.concat(sockets.flatMap((s) => s.keys)),
    open: () => sockets.filter((s) => !s.socket.isClosed()).length,
  };
}

async function axeViolations(page: Page): Promise<string[]> {
  const result = await new AxeBuilder({ page }).analyze();
  expect(result.passes.length).toBeGreaterThan(20);
  return result.violations.map((v) => `${v.id}: ${v.nodes.map((n) => n.target.join(' ')).join(', ')}`);
}

test('shows the screen of a session with a terminal; xterm loads only then', async ({ page }, info) => {
  const xterm: string[] = [];
  page.on('request', (request) => {
    if (/xterm/i.test(request.url())) xterm.push(request.url());
  });
  await page.goto(consolePath(ID.ses1));
  await expect(sessionTitle(page, 'Draft method section')).toBeVisible();
  await expect(transcript(page)).toBeVisible();
  expect(xterm).toEqual([]);

  await terminalOption(page).click();
  await expect(page).toHaveURL(atPath(terminalPath(ID.ses1)));
  await expect(frame(page)).toBeVisible();
  await expect(mode(page)).toHaveText('Viewing');
  await expect(status(page)).toHaveText('Live');
  expect(xterm.length).toBeGreaterThan(0);
  await expect(frame(page)).toHaveAttribute('data-renderer', /^(webgl|dom)$/);
  info.annotations.push({ type: 'renderer', description: (await frame(page).getAttribute('data-renderer')) ?? '' });
  await screenReaderMode(page);
  await expect(screen(page)).toContainText('Claude Code');
  await expect(screen(page)).toContainText('Draft method section');
  await expect(screen(page)).toContainText('Mock terminal: what you type is echoed back.');
  await page.screenshot({ path: info.outputPath('terminal-light.png') });

  // Back to the chat, by the switch.
  await page.getByRole('radio', { name: 'Chat' }).click();
  await expect(page).toHaveURL(atPath(consolePath(ID.ses1)));
  await expect(transcript(page)).toBeVisible();
  await expect(frame(page)).toHaveCount(0);
});

test('takes control, types and sees the echo; keys reach the program; release gives the shell its keys', async ({
  page,
}) => {
  const terminals = watchTerminals(page);
  await page.goto(terminalPath(ID.ses4));
  await screenReaderMode(page);
  await expect(screen(page)).toContainText('Mock terminal');
  await expect(status(page)).toHaveText('Live');

  // In view mode the shell keeps its keys, even with the terminal focused.
  await frame(page).focus();
  await page.keyboard.press('Control+KeyB');
  await expect(sidebar(page)).toHaveAttribute('data-collapsed', 'true');
  await page.keyboard.press('Control+KeyB');
  await expect(sidebar(page)).toHaveAttribute('data-collapsed', 'false');
  await page.keyboard.type('ignored');
  expect(terminals.keys().length).toBe(0);

  // Enter on the focused terminal takes control (and is not itself sent).
  await expect(frame(page)).toBeFocused();
  await page.keyboard.press('Enter');
  await expect(mode(page)).toHaveText('In control');
  await expect(frame(page)).toHaveAttribute('data-shell-keys', 'none');
  await page.keyboard.type('hello');
  await expect(screen(page)).toContainText('> hello');
  expect(terminals.keys().toString('latin1')).toBe('hello');

  // Esc, Ctrl K, Ctrl B and F6 go to the program, not to the shell or the console.
  await page.keyboard.press('Escape');
  await page.keyboard.press('Control+KeyK');
  await page.keyboard.press('Control+KeyB');
  await page.keyboard.press('F6');
  await expect.poll(() => terminals.keys().toString('latin1')).toBe('hello\x1b\x0b\x02\x1b[17~');
  await expect(palette(page)).toHaveCount(0);
  await expect(sidebar(page)).toHaveAttribute('data-collapsed', 'false');
  await expect(mode(page)).toHaveText('In control');

  // Ctrl+Shift+X releases, without reaching the program.
  await page.keyboard.press('Control+Shift+KeyX');
  await expect(mode(page)).toHaveText('Viewing');
  await expect(frame(page)).toBeFocused();
  await expect(frame(page)).not.toHaveAttribute('data-shell-keys', 'none');
  expect(terminals.keys().toString('latin1')).toBe('hello\x1b\x0b\x02\x1b[17~');
  await page.keyboard.press('Control+KeyK');
  await expect(palette(page)).toBeFocused();
  await page.keyboard.press('Escape');
  await expect(palette(page)).toHaveCount(0);

  // The buttons do the same.
  await page.getByRole('button', { name: 'Take control' }).click();
  await expect(mode(page)).toHaveText('In control');
  await page.keyboard.type('!');
  await expect(screen(page)).toContainText('!');
  await page.getByRole('button', { name: 'Release' }).click();
  await expect(mode(page)).toHaveText('Viewing');
  expect(terminals.open()).toBe(1);
});

test('a session that ran over a day shows the truncated marker', async ({ page }) => {
  await page.goto(terminalPath(ID.ses2));
  await expect(sessionTitle(page, 'Seed runs 1–5')).toBeVisible();
  await expect(page.getByTestId('terminal-truncated')).toHaveText('Earlier output is no longer available.');
  await screenReaderMode(page);
  await expect(screen(page)).toContainText('Codex');
});

test('a session on the unreachable machine shows the 503 reason, once', async ({ page }) => {
  // The demo workspace has no session with a terminal on the unreachable machine, so the browser
  // is told SES0005 has one; the hub still refuses its terminal with a 503.
  await page.route(
    (url) => url.pathname === `/v1/sessions/${ID.ses5}`,
    async (route) => {
      const response = await route.fetch();
      const session = (await response.json()) as Record<string, unknown>;
      await route.fulfill({ response, json: { ...session, terminal: '01JB000000000000000TRM0005' } });
    },
  );
  const terminals = watchTerminals(page);
  await page.goto(terminalPath(ID.ses5));
  await expect(sessionTitle(page, 'Try a cosine schedule')).toBeVisible();
  await expect(status(page)).toHaveText('gpu-box cannot be reached right now, so its terminal cannot be shown.');
  await expect(page.getByRole('button', { name: 'Take control' })).toBeDisabled();
  // It does not retry: one attempt, however long we wait.
  await page.waitForTimeout(3_000);
  expect(terminals.sockets).toHaveLength(1);

  // A person can ask again: one more attempt, the same answer, and no loop after it.
  await page.getByRole('button', { name: 'Try again' }).click();
  await expect.poll(() => terminals.sockets.length).toBe(2);
  await expect(status(page)).toHaveText('gpu-box cannot be reached right now, so its terminal cannot be shown.');
  await expect(frame(page)).toBeFocused();
  await page.waitForTimeout(2_000);
  expect(terminals.sockets).toHaveLength(2);

  // In a narrow console the reason wraps rather than being cut off.
  await page.setViewportSize({ width: 700, height: 800 });
  await expect(page.locator('[data-console-layout]')).toHaveAttribute('data-console-layout', 'narrow');
  await expect(status(page)).toBeVisible();
  const cut = await status(page).evaluate((element) => element.scrollWidth > element.clientWidth);
  expect(cut).toBe(false);
});

test('without a terminal the switch is disabled, with the reason', async ({ page }) => {
  await page.goto(terminalPath(ID.ses6));
  await expect(sessionTitle(page, 'Co-author responses')).toBeVisible();
  await expect(terminalOption(page)).toHaveAttribute('aria-disabled', 'true');
  await expect(terminalOption(page)).toHaveAccessibleDescription('This session has no terminal.');
  await expect(page.getByTestId('terminal-unavailable')).toBeVisible();
  // The link asked for the terminal; the chat shows instead, and choosing Terminal does nothing.
  await expect(transcript(page)).toBeVisible();
  // `force`: Playwright will not click what is aria-disabled, but a person can.
  await terminalOption(page).click({ force: true });
  await expect(frame(page)).toHaveCount(0);
  await expect(terminalOption(page)).toHaveAttribute('aria-checked', 'false');
  await terminalOption(page).focus();
  await expect(terminalOption(page)).toBeFocused();
});

test('a reload with ?view=terminal brings the screen back once, not twice', async ({ page }) => {
  await page.goto(terminalPath(ID.ses1));
  await screenReaderMode(page);
  await expect(screen(page)).toContainText('Mock terminal');

  const terminals = watchTerminals(page);
  await page.reload();
  await expect(page).toHaveURL(atPath(terminalPath(ID.ses1)));
  // The screen reader mode was remembered.
  await expect(page.getByRole('checkbox', { name: 'Screen reader mode' })).toBeChecked();
  await expect(screen(page)).toContainText('Mock terminal');
  await expect(status(page)).toHaveText('Live');
  await expect(page.locator('.xterm')).toHaveCount(1);
  const rows = await screen(page).locator('[role="listitem"]').allInnerTexts();
  expect(rows.filter((row) => row.includes('Mock terminal: what you type is echoed back.'))).toHaveLength(1);
  // One live socket (Strict Mode's first mount closes its own).
  await expect.poll(() => terminals.open()).toBe(1);
});

test('program output cannot set the title or the clipboard', async ({ page, context }) => {
  await context.grantPermissions(['clipboard-read', 'clipboard-write']);
  await page.goto(terminalPath(ID.ses1));
  await screenReaderMode(page);
  await expect(screen(page)).toContainText('Mock terminal');
  const title = await page.title();
  await page.evaluate(() => navigator.clipboard.writeText('mine'));

  await page.getByRole('button', { name: 'Take control' }).click();
  // The mock echoes what is typed, so these come back as output: OSC 0 and 2 (title), OSC 52
  // (clipboard, "hijacked" in base64).
  for (const sequence of [']0;pwned', ']2;pwned', ']52;c;aGlqYWNrZWQ=']) {
    await page.keyboard.press('Escape');
    await page.keyboard.type(sequence);
    await page.keyboard.press('Control+KeyG');
  }
  await page.keyboard.type('done');
  await expect(screen(page)).toContainText('> done');
  await expect(screen(page)).not.toContainText('pwned');
  expect(await page.title()).toBe(title);
  expect(await page.evaluate(() => navigator.clipboard.readText())).toBe('mine');
});

test('a narrow console gives the session pane to the terminal', async ({ page }, info) => {
  await page.setViewportSize({ width: 700, height: 800 });
  await page.goto(terminalPath(ID.ses1));
  await expect(page.locator('[data-console-layout]')).toHaveAttribute('data-console-layout', 'narrow');
  await expect(frame(page)).toBeVisible();
  await expect(sessionTitle(page, 'Draft method section')).toBeVisible();
  // Only the header's title row: no machine, folder or linked work above the terminal.
  await expect(page.getByText('Folder', { exact: true })).toHaveCount(0);
  await expect(page.getByRole('navigation', { name: 'Linked work' })).toHaveCount(0);
  const box = await frame(page).boundingBox();
  expect(box?.height ?? 0).toBeGreaterThan(500);
  await page.screenshot({ path: info.outputPath('terminal-narrow.png') });
});

for (const theme of ['light', 'dark'] as const) {
  test(`without WebGL, xterm's DOM renderer draws the screen, and axe passes: ${theme}`, async ({ page }, info) => {
    await page.addInitScript(() => {
      const getContext = HTMLCanvasElement.prototype.getContext;
      HTMLCanvasElement.prototype.getContext = function (this: HTMLCanvasElement, kind: string, ...rest: unknown[]) {
        return kind === 'webgl2' ? null : (getContext as (...args: unknown[]) => unknown).call(this, kind, ...rest);
      } as typeof getContext;
    });
    await page.emulateMedia({ colorScheme: theme, reducedMotion: 'reduce' });
    await page.goto(terminalPath(ID.ses1));
    await page.getByRole('radio', { name: theme === 'light' ? 'Light' : 'Dark' }).click();
    await expect(frame(page)).toHaveAttribute('data-renderer', 'dom');
    // The DOM renderer's rows are text, so the screen is readable without the screen reader mode.
    await expect(page.locator('.xterm-rows')).toContainText('Mock terminal: what you type is echoed back.');
    await page.screenshot({ path: info.outputPath(`terminal-dom-${theme}.png`) });
    expect(await axeViolations(page)).toEqual([]);
  });

  for (const layout of ['wide', 'narrow'] as const) {
    test(`axe finds no violations with the terminal: ${layout}, ${theme}`, async ({ page }, info) => {
      await page.emulateMedia({ colorScheme: theme, reducedMotion: 'reduce' });
      if (layout === 'narrow') await page.setViewportSize({ width: 700, height: 800 });
      await page.goto(terminalPath(ID.ses1));
      await page.getByRole('radio', { name: theme === 'light' ? 'Light' : 'Dark' }).click();
      await expect(page.locator('html')).toHaveAttribute('data-theme', theme);
      await expect(page.locator('[data-console-layout]')).toHaveAttribute('data-console-layout', layout);
      await expect(status(page)).toHaveText('Live');
      await page.screenshot({ path: info.outputPath(`terminal-${layout}-${theme}.png`) });
      expect(await axeViolations(page)).toEqual([]);

      // In control, with the screen reader rows on.
      await page.getByRole('button', { name: 'Take control' }).click();
      await screenReaderMode(page);
      await expect(screen(page)).toContainText('Mock terminal');
      await frame(page).focus();
      expect(await axeViolations(page)).toEqual([]);
    });
  }
}
