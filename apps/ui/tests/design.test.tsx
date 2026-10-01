// @vitest-environment happy-dom

import { act, cleanup, fireEvent, render, screen } from '@testing-library/react';
import axe from 'axe-core';
import { useState } from 'react';
import { afterEach, beforeEach, describe, expect, it, vi } from 'vitest';
import { Avatar, avatarLabel, initials } from '../src/design/avatar.tsx';
import { Popover } from '../src/design/popover.tsx';
import { ResizablePanel } from '../src/design/resizable-panel.tsx';
import { Tree, TreeItem } from '../src/design/tree.tsx';

afterEach(cleanup);

async function violations(): Promise<string[]> {
  const result = await axe.run(
    { exclude: [['[data-radix-focus-guard]']] },
    { rules: { 'color-contrast': { enabled: false } } },
  );
  return result.violations.map(
    (v) => `${v.id} (${v.impact ?? '?'}): ${v.help} at ${v.nodes.map((n) => JSON.stringify(n.target)).join(', ')}`,
  );
}

function Projects() {
  const [open, setOpen] = useState<Record<string, boolean>>({});
  const item = (value: string, children: string[] = []) => (
    <TreeItem
      key={value}
      value={value}
      level={1}
      expanded={children.length > 0 ? (open[value] ?? false) : undefined}
      onExpandedChange={(next) => setOpen({ ...open, [value]: next })}
      items={children.map((child) => (
        <TreeItem key={child} value={child} level={2}>
          <a href={`#${child}`}>{child}</a>
        </TreeItem>
      ))}
    >
      <a href={`#${value}`}>{value}</a>
    </TreeItem>
  );
  return (
    <Tree label="Projects" defaultValue="Paper">
      {item('Paper', ['Submission', 'Seed runs'])}
      {item('Tooling', ['Parsers'])}
      {item('Thesis')}
    </Tree>
  );
}

const treeitem = (name: string) => screen.getByRole('treeitem', { name });

describe('Tree', () => {
  it('has one tab stop and moves, opens and closes with the arrow keys', () => {
    render(<Projects />);
    expect(screen.getAllByRole('treeitem').map((el) => el.tabIndex)).toEqual([0, -1, -1]);
    const paper = treeitem('Paper');
    paper.focus();

    fireEvent.keyDown(paper, { key: 'ArrowRight' });
    expect(paper.getAttribute('aria-expanded')).toBe('true');
    const group = document.getElementById(paper.getAttribute('aria-owns') ?? '');
    expect(group?.getAttribute('role')).toBe('group');

    fireEvent.keyDown(paper, { key: 'ArrowRight' });
    expect(document.activeElement).toBe(treeitem('Submission'));
    expect(treeitem('Submission').tabIndex).toBe(0);
    expect(paper.tabIndex).toBe(-1);

    fireEvent.keyDown(document.activeElement as Element, { key: 'ArrowDown' });
    expect(document.activeElement).toBe(treeitem('Seed runs'));
    fireEvent.keyDown(document.activeElement as Element, { key: 'ArrowDown' });
    expect(document.activeElement).toBe(treeitem('Tooling'));
    fireEvent.keyDown(document.activeElement as Element, { key: 'End' });
    expect(document.activeElement).toBe(treeitem('Thesis'));
    fireEvent.keyDown(document.activeElement as Element, { key: 'Home' });
    expect(document.activeElement).toBe(paper);
    fireEvent.keyDown(paper, { key: 's' });
    expect(document.activeElement).toBe(treeitem('Submission'));

    fireEvent.keyDown(document.activeElement as Element, { key: 'ArrowLeft' });
    expect(document.activeElement).toBe(paper);
    fireEvent.keyDown(paper, { key: 'ArrowLeft' });
    expect(paper.getAttribute('aria-expanded')).toBe('false');
    expect(screen.queryByRole('treeitem', { name: 'Submission' })).toBeNull();
  });

  it('moves focus to the item when a group holding focus is closed', () => {
    render(<Projects />);
    const paper = treeitem('Paper');
    fireEvent.keyDown(paper, { key: 'ArrowRight' });
    treeitem('Seed runs').focus();
    const chevron = paper.querySelector('[aria-hidden]');
    if (chevron === null) throw new Error('no chevron');
    fireEvent.click(chevron);
    expect(paper.getAttribute('aria-expanded')).toBe('false');
    expect(document.activeElement).toBe(paper);
    expect(paper.tabIndex).toBe(0);
  });
});

function Panel() {
  const [width, setWidth] = useState(360);
  return (
    <ResizablePanel label="Orchestrator" width={width} onWidthChange={setWidth} min={280} max={400}>
      <p>content</p>
    </ResizablePanel>
  );
}

describe('ResizablePanel', () => {
  it('resizes from the keyboard within its bounds', () => {
    render(<Panel />);
    const edge = screen.getByRole('separator', { name: 'Resize Orchestrator' });
    const panel = screen.getByRole('complementary', { name: 'Orchestrator' });
    expect(edge.getAttribute('aria-controls')).toBe(panel.id);
    fireEvent.keyDown(edge, { key: 'ArrowLeft' });
    expect(edge.getAttribute('aria-valuenow')).toBe('376');
    expect(panel.style.width).toBe('376px');
    fireEvent.keyDown(edge, { key: 'ArrowLeft' });
    fireEvent.keyDown(edge, { key: 'ArrowLeft' });
    expect(edge.getAttribute('aria-valuenow')).toBe('400');
    fireEvent.keyDown(edge, { key: 'Home' });
    expect(edge.getAttribute('aria-valuenow')).toBe('280');
    fireEvent.keyDown(edge, { key: 'ArrowRight' });
    expect(edge.getAttribute('aria-valuenow')).toBe('280');
  });
});

describe('Avatar', () => {
  const sam = { kind: 'human', name: 'Sam Rivera' } as const;
  const writer = { kind: 'agent', name: 'Writer' } as const;

  it('names people plainly and agents with their owner', () => {
    expect(initials('Sam Rivera')).toBe('SR');
    expect(initials('Back office')).toBe('BO');
    expect(initials('writer')).toBe('W');
    expect(avatarLabel(sam)).toBe('Sam Rivera');
    expect(avatarLabel(writer)).toBe('Writer (agent)');
    render(<Avatar member={writer} owner={sam} />);
    expect(screen.getByRole('img', { name: 'Writer (agent of Sam Rivera)' }).textContent).toBe('WS');
  });
});

function Evidence({ label }: { label: string }) {
  return (
    <Popover label={`${label}, with evidence`} content={<p>Evidence for {label}</p>} contentLabel={`Evidence for “${label}”`}>
      {label}
    </Popover>
  );
}

describe('Popover', () => {
  beforeEach(() => {
    // What index.html gives the real page; axe flags their absence otherwise.
    document.title = 'PitCrew';
    document.documentElement.lang = 'en';
  });

  it('has no axe violations closed or open', async () => {
    render(
      <main>
        <h1>Notes</h1>
        <p>
          <Evidence label="moved PAP-1" /> happened.
        </p>
      </main>,
    );
    expect(await violations()).toEqual([]);
    fireEvent.click(screen.getByRole('button', { name: 'moved PAP-1, with evidence' }));
    await screen.findByRole('dialog', { name: 'Evidence for “moved PAP-1”' });
    expect(await violations()).toEqual([]);
  });

  it('previews on focus (inert) without moving focus, opens with focus moved in, and Escape returns it', async () => {
    render(<Evidence label="moved PAP-1" />);
    const trigger = screen.getByRole('button', { name: 'moved PAP-1, with evidence' });
    expect(trigger.getAttribute('aria-expanded')).toBe('false');

    // Focus previews it, inert, without moving focus off the trigger.
    act(() => trigger.focus());
    const preview = await screen.findByRole('dialog', { name: 'Evidence for “moved PAP-1”' });
    expect(preview.hasAttribute('inert')).toBe(true);
    expect(document.activeElement).toBe(trigger);

    // Enter opens it: focus moves in.
    fireEvent.keyDown(trigger, { key: 'Enter' });
    expect(trigger.getAttribute('aria-expanded')).toBe('true');
    const dialog = screen.getByRole('dialog', { name: 'Evidence for “moved PAP-1”' });
    await vi.waitFor(() => expect(document.activeElement).toBe(dialog));
    expect(dialog.hasAttribute('inert')).toBe(false);
    expect(trigger.getAttribute('aria-controls')).toBe(dialog.id);

    // Escape closes it and returns focus to the trigger, without previewing it again.
    act(() => dialog.focus());
    fireEvent.keyDown(dialog, { key: 'Escape' });
    await vi.waitFor(() => expect(screen.queryByRole('dialog')).toBeNull());
    expect(document.activeElement).toBe(trigger);
    expect(trigger.getAttribute('aria-expanded')).toBe('false');
  });

  it('previews on hover, and the pointer can move in to follow something in the content', async () => {
    render(<Evidence label="ran a tool" />);
    const trigger = screen.getByRole('button', { name: 'ran a tool, with evidence' });
    fireEvent.pointerEnter(trigger);
    const preview = await screen.findByRole('dialog', { name: 'Evidence for “ran a tool”' });
    expect(preview.hasAttribute('inert')).toBe(false);
    expect(trigger.getAttribute('aria-expanded')).toBe('false');

    // Moving from the trigger into the content keeps it open.
    fireEvent.pointerLeave(trigger);
    fireEvent.pointerEnter(preview);
    await new Promise((done) => setTimeout(done, 400));
    expect(screen.getByRole('dialog')).toBe(preview);

    fireEvent.pointerLeave(preview);
    await vi.waitFor(() => expect(screen.queryByRole('dialog')).toBeNull());
  });

  it('a second click toggles it closed', async () => {
    render(<Evidence label="moved PAP-1" />);
    const trigger = screen.getByRole('button', { name: 'moved PAP-1, with evidence' });
    fireEvent.click(trigger);
    await screen.findByRole('dialog');
    fireEvent.click(trigger);
    await vi.waitFor(() => expect(screen.queryByRole('dialog')).toBeNull());
  });
});
