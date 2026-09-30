// @vitest-environment happy-dom
// @vitest-environment-options {"url": "http://localhost:5173/"}

import { cleanup, fireEvent, render, screen, within } from '@testing-library/react';
import { useState } from 'react';
import { afterEach, beforeEach, describe, expect, it, vi } from 'vitest';
import type { Project, Session, Workstream } from '../../data/index.ts';
import { NO_FACETS, type SessionFacets } from '../facets.ts';
import { SessionFilters } from '../session-filters.tsx';
import { SessionList, SessionListView } from '../session-list.tsx';
import {
  eventually,
  ID,
  otherClient,
  renderWithHub,
  scrollTo,
  startHub,
  stubLayout,
  unmountAndSettle,
  type HubProcess,
} from './harness.tsx';

/** The list as the user reads it: group headers and session ids, in order. */
function outline(container: HTMLElement): string[] {
  return [...container.querySelectorAll<HTMLElement>('[data-index]')]
    .sort((a, b) => Number(a.dataset.index) - Number(b.dataset.index))
    .map((row) => {
      const option = row.querySelector<HTMLElement>('[role="option"]');
      return option === null ? `# ${row.textContent?.replace(/\d+$/, '').trim()}` : (option.dataset.session ?? '?');
    });
}

const rowOf = (id: string) => document.querySelector<HTMLElement>(`[data-session="${id}"]`);

describe('SessionList against the mock hub', () => {
  let hub: HubProcess | undefined;
  let unstub: () => void = () => {};

  beforeEach(() => {
    unstub = stubLayout({ viewport: 2_000, row: 56 });
  });

  afterEach(async () => {
    await unmountAndSettle();
    unstub();
    await hub?.close();
    hub = undefined;
  });

  it('groups the demo sessions by project and workstream, plus Unsorted', async () => {
    hub = await startHub();
    const { container } = renderWithHub(hub, <SessionList onSelect={() => {}} />);
    await screen.findByRole('listbox', { name: 'Sessions' });
    expect(outline(container)).toEqual([
      '# Paper · Diffusion study',
      '# Submission',
      ID.ses1,
      ID.ses6, // ended sessions sort last
      '# Seed runs',
      ID.ses2,
      '# Tooling',
      '# Parsers',
      ID.ses3, // latest activity first
      ID.ses4,
      '# Unsorted',
      ID.ses5,
    ]);
    // The agent's handle comes from the members query, which may land after the list.
    await eventually(() => {
      const text = rowOf(ID.ses1)?.textContent ?? '';
      for (const part of ['Claude', 'Draft method section', '@writer', 'Working', 'Editing method.tex (§3.2)']) {
        expect(text).toContain(part);
      }
    });
    expect(rowOf(ID.ses5)?.textContent).toContain('Unreachable');
    expect(rowOf(ID.ses6)?.textContent).toContain('Ended');
    expect(rowOf(ID.ses3)?.textContent).toContain('Waiting');
    expect(rowOf(ID.ses4)?.textContent).toContain('Idle');
  });

  it('updates a row live when its session changes state, and shows a starting session', async () => {
    hub = await startHub();
    renderWithHub(hub, <SessionList onSelect={() => {}} />);
    await screen.findByRole('listbox', { name: 'Sessions' });
    expect(rowOf(ID.ses4)?.dataset.state).toBe('idle');

    const api = otherClient(hub);
    await api.request('POST', `/v1/sessions/${ID.ses4}/send`, { body: { text: 'Summarise the change' } });
    await eventually(() => expect(rowOf(ID.ses4)?.dataset.state).toBe('working'), { timeout: 4_000 });
    expect(rowOf(ID.ses4)?.textContent).toContain('Thinking');

    const started = await api.request<Session>('POST', '/v1/sessions', {
      body: { machine: ID.laptop, engine: 'codex', cwd: '/home/sam/scratch' },
    });
    await eventually(() => expect(within(rowOf(started.id) as HTMLElement).getByTestId('starting')).toBeTruthy(), {
      timeout: 4_000,
    });
    expect(rowOf(started.id)?.textContent).toContain('scratch');
    // Unlinked and in no project's folder: Unsorted.
    await eventually(() => expect(rowOf(started.id)?.dataset.state).toBe('working'), { timeout: 5_000 });
  }, 15_000);

  it('moves with the keyboard and selects with Enter or a click', async () => {
    hub = await startHub();
    const onSelect = vi.fn<(session: Session) => void>();
    renderWithHub(hub, <SessionList onSelect={onSelect} />);
    const listbox = await screen.findByRole('listbox', { name: 'Sessions' });
    listbox.focus();
    fireEvent.focus(listbox);
    expect(listbox.getAttribute('aria-activedescendant')).toContain(ID.ses1);
    fireEvent.keyDown(listbox, { key: 'ArrowDown' });
    expect(listbox.getAttribute('aria-activedescendant')).toContain(ID.ses6);
    fireEvent.keyDown(listbox, { key: 'End' });
    expect(listbox.getAttribute('aria-activedescendant')).toContain(ID.ses5);
    fireEvent.keyDown(listbox, { key: 'Enter' });
    expect(onSelect).toHaveBeenLastCalledWith(expect.objectContaining({ id: ID.ses5 }));
    fireEvent.keyDown(listbox, { key: 'Home' });
    fireEvent.keyDown(listbox, { key: 'ArrowUp' });
    expect(listbox.getAttribute('aria-activedescendant')).toContain(ID.ses1);

    fireEvent.click(rowOf(ID.ses2) as HTMLElement);
    expect(onSelect).toHaveBeenLastCalledWith(expect.objectContaining({ id: ID.ses2 }));
  });

  it('filters by facets', async () => {
    hub = await startHub();
    function Pane() {
      const [facets, setFacets] = useState<SessionFacets>(NO_FACETS);
      return (
        <>
          <SessionFilters value={facets} onChange={setFacets} />
          <SessionList facets={facets} onSelect={() => {}} />
        </>
      );
    }
    const { container } = renderWithHub(hub, <Pane />);
    await screen.findByRole('listbox', { name: 'Sessions' });
    fireEvent.click(await screen.findByRole('checkbox', { name: /Codex/ }));
    await eventually(() => expect(outline(container).filter((r) => !r.startsWith('#'))).toEqual([ID.ses2]));

    fireEvent.click(screen.getByRole('checkbox', { name: /Codex/ }));
    fireEvent.click(screen.getByRole('checkbox', { name: /^Unsorted/ }));
    await eventually(() => expect(outline(container)).toEqual(['# Unsorted', ID.ses5]));

    fireEvent.click(screen.getByRole('button', { name: 'Clear' }));
    await eventually(() => expect(outline(container).filter((r) => !r.startsWith('#'))).toHaveLength(6));
  });
});

describe('SessionListView with 10,000 sessions', () => {
  afterEach(() => cleanup());

  it('keeps the DOM bounded while scrolling', () => {
    const unstub = stubLayout({ viewport: 600, row: 56 });
    try {
      const project: Project = { id: 'P', key: 'P', name: 'Big', status: 'in_progress', lead: 'M', members: [], external: [] };
      const workstreams: Workstream[] = Array.from({ length: 10 }, (_, w) => ({
        id: `W${w}`,
        project: 'P',
        name: `Stream ${w}`,
        status: 'active',
        health: 'on_track',
        locations: [],
        external: [],
      }));
      const sessions: Session[] = Array.from({ length: 10_000 }, (_, i) => ({
        id: `S${String(i).padStart(5, '0')}`,
        engine: 'claude',
        native_id: `n${i}`,
        machine: 'M',
        cwd: `/work/${i}`,
        title: `Session ${i}`,
        workstream: `W${i % 10}`,
        state: 'idle',
        started: 0,
        last_activity: 1_000_000 - i,
      }));
      const { container } = render(
        <SessionListView
          sessions={sessions}
          places={{ projects: [project], workstreams, tasks: [] }}
          onSelect={() => {}}
        />,
      );
      const options = () => container.querySelectorAll('[role="option"]').length;
      expect(options()).toBeGreaterThan(5);
      expect(options()).toBeLessThan(40);

      const scroller = container.querySelector<HTMLElement>('[data-virtual-scroller]') as HTMLElement;
      scrollTo(scroller, 300_000);
      const shown = [...container.querySelectorAll<HTMLElement>('[role="option"]')].map((o) => o.dataset.session);
      expect(options()).toBeLessThan(40);
      expect(shown).not.toContain('S00000');
      expect(shown.length).toBeGreaterThan(5);
    } finally {
      unstub();
    }
  });
});
