// @vitest-environment happy-dom
// @vitest-environment-options {"url": "http://localhost:5173/"}

import { fireEvent, screen, within } from '@testing-library/react';
import { afterEach, beforeEach, describe, expect, it } from 'vitest';
import { Home } from '../home.tsx';
import { ProjectOverview, WorkstreamOverview } from '../overview.tsx';
import { AGENT_TOKEN, demo, eventually, otherClient, renderWithHub, startHub, stopHub, type Hub } from './harness.tsx';

describe('ProjectOverview and WorkstreamOverview', () => {
  let hub: Hub;

  beforeEach(async () => {
    hub = await startHub();
  });

  afterEach(async () => {
    await stopHub(hub);
  });

  it('shows where the project stands, its workstreams, what needs you, agents and activity', async () => {
    const opened: string[] = [];
    renderWithHub(<ProjectOverview project={demo.paper} />, hub, {
      nav: { openWorkstream: (w) => opened.push(w) },
    });
    await screen.findByRole('heading', { level: 1, name: 'Paper · Diffusion study' });
    const stands = await screen.findByRole('region', { name: 'Where the project stands' });
    // The PAP project's pending proposal repeats its own text, so it shows up twice (its own
    // "Proposed update" box, below the brief in force) — see where-it-stands.test.tsx.
    expect(within(stands).getAllByText(/The method section is about half drafted/)).toHaveLength(2);

    const table = await screen.findByRole('table', { name: 'Workstreams' });
    await within(table).findByText('Rerun or drop seed 3.');
    const rows = within(table)
      .getAllByRole('row')
      .slice(1)
      .map((row) => Array.from(row.children, (cell) => cell.textContent));
    expect(rows).toEqual([
      ['Submission', 'Active', 'On track', 'Review responses.md.', '3'],
      ['Seed runs', 'Active', 'At risk', 'Rerun or drop seed 3.', '3'],
      ['Ablation: noise schedule', 'Idea', 'On track', '—', '0'],
    ]);
    fireEvent.click(within(table).getByRole('button', { name: 'Seed runs' }));
    expect(opened).toEqual([demo.seedRuns]);

    const needs = screen.getByRole('region', { name: 'Needs you' });
    await within(needs).findByText('Seed 3 diverged at epoch 9. Rerun or drop it?');
    expect(within(needs).getByText('Co-author responses are ready for review')).toBeTruthy();
    expect(within(needs).queryByText('Merge the benchmark change into parsers?')).toBeNull();

    const agents = screen.getByRole('region', { name: 'Agents on it' });
    await within(agents).findByText('Watching job 4815162 · epoch 12/40');
    expect(within(agents).getByText('Editing method.tex (§3.2)')).toBeTruthy();
    expect(within(agents).queryByText('Asks: merge the benchmark change?')).toBeNull();

    const activity = screen.getByRole('region', { name: 'Recent activity' });
    const events = await within(activity).findByRole('list', { name: 'Events' });
    expect(within(events).getByText('moved PAP-3 from In progress to Review')).toBeTruthy();
    expect(within(events).queryByText(/Parse Codex/)).toBeNull();
    // Summary: the project's day paragraphs (see recaps.test.tsx).
    fireEvent.click(within(activity).getByRole('radio', { name: 'Summary' }));
    await within(activity).findByRole('list', { name: 'Days' });
    expect(within(activity).queryByRole('list', { name: 'Events' })).toBeNull();
  });

  it('shows a workstream with its tasks and activity, live', async () => {
    renderWithHub(<WorkstreamOverview workstream={demo.seedRuns} />, hub);
    await screen.findByRole('heading', { level: 1, name: 'Seed runs' });
    await screen.findByText(/Four of five seeds are healthy/);
    const tasks = screen.getByRole('list', { name: 'Tasks by status' });
    await within(tasks).findByText('In progress 1');
    expect(within(tasks).getByText('Todo 1')).toBeTruthy();
    expect(within(tasks).getByText('Backlog 1')).toBeTruthy();

    await otherClient(hub).request('PATCH', `/v1/workstreams/${demo.seedRuns}`, { body: { health: 'blocked' } });
    await screen.findByText('Blocked');
    const activity = screen.getByRole('region', { name: 'Recent activity' });
    await within(activity).findByText('marked Seed runs active, blocked');
  });
});

describe('Home', () => {
  let hub: Hub;

  beforeEach(async () => {
    localStorage.clear();
    hub = await startHub();
  });

  afterEach(async () => {
    await stopHub(hub);
  });

  it('shows where things stand, what needs you and the agents at work', async () => {
    renderWithHub(<Home />, hub);
    const stand = await screen.findByRole('region', { name: 'Where things stand' });
    await within(stand).findByText(/The method section is about half drafted/);
    expect(within(stand).getByText('Tooling')).toBeTruthy();
    await within(stand).findByText('6 open tasks · 1 workstream at risk or blocked');
    expect(within(stand).getByText('3 open tasks')).toBeTruthy();

    const needs = screen.getByRole('region', { name: 'Needs you' });
    await within(needs).findByText('3 open asks');

    const agents = screen.getByRole('region', { name: 'Agents now' });
    await within(agents).findByText('Asks: merge the benchmark change?');
    // Waiting sessions come first.
    expect(within(agents).getAllByRole('listitem')[0]?.textContent).toContain('@reviewer');
  });

  it('shows what changed since I last looked', async () => {
    renderWithHub(<Home />, hub);
    const since = await screen.findByRole('region', { name: 'Since you last looked' });
    const changes = await within(since).findByRole('list', { name: 'Changes' });
    // Fifteen events: @sam's own four (three dispatches, a brief) are left out, and each
    // session's events are a line of their own, named by its agent.
    expect(within(changes).getAllByRole('listitem')).toHaveLength(11);
    expect(within(since).getByText('11 new changes')).toBeTruthy();
    expect(within(changes).queryByText('@sam')).toBeNull();
    // Newest first, by when it happened.
    const items = within(changes).getAllByRole('listitem');
    const times = items.map((li) => Date.parse(li.querySelector('time')?.getAttribute('datetime') ?? ''));
    expect(times).toEqual([...times].sort((a, b) => b - a));
    fireEvent.click(await within(since).findByRole('button', { name: 'Mark all as read' }));
    await within(since).findByText('Nothing new since you last looked.');
    expect(await otherClient(hub).request('GET', '/v1/me/cursors')).toEqual([{ scope: 'workspace', rev: 15 }]);

    // My own change is not news; the agent's is.
    await otherClient(hub).moveTask('PAP-5', 'in_progress');
    await otherClient(hub, AGENT_TOKEN).moveTask('PAP-2', 'in_progress');
    const fresh = await within(since).findByRole('list', { name: 'Changes' });
    await eventually(() => expect(within(fresh).getAllByRole('listitem')).toHaveLength(1));
    expect(within(fresh).getByText('moved PAP-2 from Todo to In progress')).toBeTruthy();
  });
});
