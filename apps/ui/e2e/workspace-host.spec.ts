import { expect, test, type Locator } from '@playwright/test';
import { installFakeDesktop } from './fake-desktop';
import { HUB_AUTH, HUB_TOKEN, HUB_URL } from './helpers';

async function expectHostFullyVisible(container: Locator, hostname: string) {
  const host = container.locator('[data-workspace-host]');
  await expect(host).toBeVisible();
  await expect(host).toHaveText(`· ${hostname}`);
  const fits = await host.evaluate((element) => {
    const box = element.getBoundingClientRect();
    if (box.width === 0 || box.height === 0 || element.scrollWidth > element.clientWidth) return false;
    for (let parent = element.parentElement; parent !== null; parent = parent.parentElement) {
      const bounds = parent.getBoundingClientRect();
      if (box.left < bounds.left - 1 || box.right > bounds.right + 1 || box.top < bounds.top - 1 || box.bottom > bounds.bottom + 1) return false;
    }
    return true;
  });
  expect(fits).toBe(true);
}

for (const hostname of ['login.example.org', 'h'.repeat(48) + '.example.org']) {
  test(`an 80-character hub name cannot hide a ${hostname.length}-character trusted host`, async ({ page, request }) => {
    const response = await request.get(`${HUB_URL}/v1/workspace`, { headers: HUB_AUTH });
    const info = await response.json() as { workspace: { id: string } };
    const name = 'This computer'.padEnd(80, 'x');
    await page.addInitScript(installFakeDesktop, {
      hubUrl: HUB_URL, token: HUB_TOKEN, jobScript: '',
      initialWorkspace: { id: info.workspace.id, name, host: hostname, kind: 'remote' as const, state: 'ready' as const },
    });
    await page.goto(`/w/${info.workspace.id}/home`);
    const switcher = page.getByRole('button', { name: `Workspace: ${name} · ${hostname}`, exact: true });
    await expectHostFullyVisible(switcher, hostname);
    await expectHostFullyVisible(page.getByRole('navigation', { name: 'Breadcrumb' }), hostname);
    await switcher.click();
    await expectHostFullyVisible(page.getByRole('menuitemradio', { name: `${name} · ${hostname}`, exact: true }), hostname);
  });
}
