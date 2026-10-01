// @vitest-environment happy-dom
// @vitest-environment-options {"url": "http://localhost:5173/"}
//
// axe on the projects components, rendered against the mock hub the way the shell will show them
// (inside <main>, under the page's <h1>). happy-dom has no layout, so colour contrast is left to the
// shell's Playwright axe run in a real browser.

import { fireEvent, screen, within } from '@testing-library/react';
import axe from 'axe-core';
import type { ReactNode } from 'react';
import { afterEach, beforeEach, describe, expect, it } from 'vitest';
import { Board } from '../board.tsx';
import { Home } from '../home.tsx';
import { Inbox } from '../inbox.tsx';
import { ProjectOverview, WorkstreamOverview } from '../overview.tsx';
import { TaskDrawer } from '../task-drawer.tsx';
import { demo, renderWithHub, startHub, stopHub, type Hub } from './harness.tsx';

/**
 * The whole document, as axe sees it. Radix's focus guards are left out: they are invisible,
 * focusable spans that exist only to send focus back into an open dialog, and `aria-hidden` marks
 * them hidden together with the rest of the page. `modal` also turns off the page-level landmark and
 * `h1` rules, since a modal dialog hides the page (and its `main` and `h1`) by design.
 */
async function violations({ modal = false }: { modal?: boolean } = {}): Promise<string[]> {
  const rules: Record<string, { enabled: boolean }> = { 'color-contrast': { enabled: false } };
  if (modal) {
    rules['landmark-one-main'] = { enabled: false };
    rules['page-has-heading-one'] = { enabled: false };
  }
  const result = await axe.run({ exclude: [['[data-radix-focus-guard]']] }, { rules });
  return result.violations.map(
    (v) => `${v.id} (${v.impact ?? '?'}): ${v.help} at ${v.nodes.map((n) => JSON.stringify(n.target)).join(', ')}`,
  );
}

const page = (title: string, content: ReactNode) => (
  <main>
    <h1>{title}</h1>
    {content}
  </main>
);

describe('accessibility (axe)', () => {
  let hub: Hub;

  beforeEach(async () => {
    // What index.html gives the real page.
    document.title = 'PitCrew';
    document.documentElement.lang = 'en';
    hub = await startHub();
  });

  afterEach(async () => {
    await stopHub(hub);
  });

  it('Board, by status and by workstream', async () => {
    renderWithHub(page('Paper', <Board project={demo.paper} />), hub);
    await screen.findByText('Editing method.tex (§3.2)');
    await screen.findAllByText('Needs you');
    expect(await violations()).toEqual([]);
    fireEvent.click(screen.getByRole('radio', { name: 'By workstream' }));
    await screen.findByRole('region', { name: 'Seed runs' });
    expect(await violations()).toEqual([]);
  });

  it('Board with a card picked up and a refused move', async () => {
    renderWithHub(page('Paper', <Board project={demo.paper} />), hub, { token: 'dev-agent-token' });
    await screen.findByText('Respond to co-author comments');
    const handle = screen.getByRole('button', { name: 'Move PAP-3, now Review' });
    fireEvent.click(handle);
    fireEvent.keyDown(handle, { key: 'ArrowRight' });
    expect(await violations()).toEqual([]);
    fireEvent.click(handle);
    await screen.findByRole('alert');
    expect(await violations()).toEqual([]);
  });

  it('TaskDrawer', async () => {
    renderWithHub(page('Paper', <TaskDrawer taskId={demo.pap1} open onOpenChange={() => {}} />), hub);
    const dialog = await screen.findByRole('dialog', { name: 'Draft the method section' });
    await within(dialog).findByText('Editing method.tex (§3.2)');
    await within(dialog).findAllByText('Agent plan · @writer');
    await within(dialog).findByRole('list', { name: 'History' });
    expect(await violations({ modal: true })).toEqual([]);
  });

  it(
    'Inbox',
    async () => {
      renderWithHub(
        <main>
          <Inbox />
        </main>,
        hub,
      );
      // The question card is a lazy chunk (stream M's); the first load in this file can be slow.
      await screen.findByText('Merge the benchmark change into parsers?', {}, { timeout: 15_000 });
      await screen.findByText('PAP-5');
      expect(await violations()).toEqual([]);
    },
    20_000,
  );

  it('ProjectOverview, WorkstreamOverview and Home', async () => {
    const { unmount } = renderWithHub(
      <main>
        <ProjectOverview project={demo.paper} />
      </main>,
      hub,
    );
    await screen.findByText('Rerun or drop seed 3.');
    await screen.findByRole('list', { name: 'Events' });
    expect(await violations()).toEqual([]);
    unmount();

    const second = renderWithHub(
      <main>
        <WorkstreamOverview workstream={demo.seedRuns} />
      </main>,
      hub,
    );
    await screen.findByText(/Four of five seeds are healthy/);
    expect(await violations()).toEqual([]);
    second.unmount();

    renderWithHub(
      <main>
        <Home />
      </main>,
      hub,
    );
    await screen.findByText('3 open asks');
    await screen.findByRole('list', { name: 'Changes' });
    expect(await violations()).toEqual([]);
  });
});
