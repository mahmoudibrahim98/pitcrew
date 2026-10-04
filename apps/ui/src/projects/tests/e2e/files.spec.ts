import AxeBuilder from '@axe-core/playwright';
import { expect, test } from '@playwright/test';

const STREAM = '01JB000000000000000WST0001';
const HUB = `http://127.0.0.1:${process.env.E2E_HUB_PORT ?? 47482}`;
const headers = { Authorization: 'Bearer dev-device-token' };
const route = (path: string) => `${HUB}/v1/workstreams/${STREAM}/files/content?loc=0&path=${encodeURIComponent(path)}`;

for (const theme of ['light', 'dark'] as const) {
  test(`Files browse, edit, conflict, image and axe in ${theme}`, async ({ page, request }) => {
    const imagePath = `pixel-${theme}.png`;
    const image = await request.put(route(imagePath), { headers, data: {
      revision: null, encoding: 'base64', content: 'iVBORw0KGgoAAAANSUhEUgAAAAEAAAABCAQAAAC1HAwCAAAAC0lEQVR42mP8/x8AAwMCAO+a2ioAAAAASUVORK5CYII=',
    } });
    expect(image.ok()).toBe(true);
    await page.goto(`/w/01JB000000000000000WSP0001/projects/01JB000000000000000PRJ0001/workstreams/${STREAM}`);
    await page.getByRole('radio', { name: theme === 'light' ? 'Light' : 'Dark', exact: true }).click();
    await page.getByRole('radio', { name: 'Files', exact: true }).click();
    const tree = page.getByRole('navigation', { name: 'Folder tree' });
    await expect(tree.getByText('outside (link, cannot open)')).toBeVisible();
    const folder = tree.getByRole('button', { name: '▸ src' });
    await folder.focus();
    await page.keyboard.press('ArrowRight');
    await expect(tree.getByRole('button', { name: 'hello.txt' })).toBeVisible();
    await page.keyboard.press('ArrowDown');
    await expect(tree.getByRole('button', { name: 'hello.txt' })).toBeFocused();
    await page.keyboard.press('Enter');
    await page.getByRole('button', { name: 'Edit', exact: true }).click();
    await page.getByLabel('Edit file text').fill(`saved ${theme}\n`);
    await page.getByRole('button', { name: 'Save', exact: true }).click();
    await expect(page.getByText('Saved', { exact: true })).toBeVisible();
    expect((await (await request.get(route('src/hello.txt'), { headers })).json()).content).toBe(`saved ${theme}\n`);
    await page.getByRole('button', { name: 'Edit', exact: true }).click();
    await page.getByLabel('Edit file text').fill('<script>literal draft</script>\n');
    page.once('dialog', (dialog) => dialog.dismiss());
    await page.getByRole('radio', { name: 'Tasks', exact: true }).click();
    await expect(page.getByLabel('Edit file text')).toHaveValue('<script>literal draft</script>\n');
    page.once('dialog', (dialog) => dialog.dismiss());
    await tree.getByRole('button', { name: imagePath }).click();
    await expect(page.getByLabel('Edit file text')).toBeVisible();
    const current = await (await request.get(route('src/hello.txt'), { headers })).json();
    expect((await request.put(route('src/hello.txt'), { headers, data: { revision: current.revision, encoding: 'utf8', content: 'external change' } })).ok()).toBe(true);
    await page.getByRole('button', { name: 'Save', exact: true }).click();
    await expect(page.getByText('Changed since you opened it')).toBeVisible();
    expect((await new AxeBuilder({ page }).analyze()).violations).toEqual([]);
    await page.getByRole('button', { name: 'Overwrite', exact: true }).click();
    await expect(page.getByText('Saved', { exact: true })).toBeVisible();
    await expect(page.getByLabel('File text')).toContainText('<script>literal draft</script>');
    expect(await page.getByLabel('File text').locator('script').count()).toBe(0);
    expect((await new AxeBuilder({ page }).analyze()).violations).toEqual([]);
    await tree.getByRole('button', { name: imagePath }).click();
    const img = page.getByRole('img', { name: imagePath });
    await expect(img).toBeVisible();
    expect(await img.evaluate((element: HTMLImageElement) => element.naturalWidth)).toBe(1);
    expect((await new AxeBuilder({ page }).analyze()).violations).toEqual([]);
    await tree.getByRole('button', { name: 'large.bin' }).click();
    await expect(page.getByText('Too large to show (8388609 bytes)')).toBeVisible();
  });
}
