import AxeBuilder from '@axe-core/playwright';
import { expect, type Page } from '@playwright/test';

/**
 * axe on the page as it is, in the light and then the dark scheme (the app's theme left on
 * "System", so its tokens follow the emulated scheme). Colours fade between schemes (buttons have
 * `transition-colors`), so each scan waits for running transitions to end first: a scan mid-fade
 * reports contrast that no one ever sees.
 */
export async function expectNoAxeViolations(page: Page, label: string): Promise<void> {
  for (const colorScheme of ['light', 'dark'] as const) {
    await page.emulateMedia({ colorScheme });
    // Finite ones only: a spinner's never finishes.
    await page.evaluate(() =>
      Promise.all(
        document
          .getAnimations()
          .filter((animation) => animation.effect?.getComputedTiming().endTime !== Infinity)
          .map((animation) => animation.finished.catch(() => undefined)),
      ),
    );
    const result = await new AxeBuilder({ page }).analyze();
    expect(
      result.violations.map((v) => `${v.id}: ${v.nodes.map((n) => n.target.join(' ')).join(', ')}`),
      `${label} (${colorScheme})`,
    ).toEqual([]);
  }
}
