// @vitest-environment happy-dom

import { cleanup, fireEvent, render, screen } from '@testing-library/react';
import { useState } from 'react';
import { afterEach, describe, expect, it } from 'vitest';
import { Avatar, avatarLabel, initials } from '../src/design/avatar.tsx';
import { ResizablePanel } from '../src/design/resizable-panel.tsx';
import { Tree, TreeItem } from '../src/design/tree.tsx';

afterEach(cleanup);

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
