// @vitest-environment happy-dom
// The workbench's layout model (no React) and its storage per workspace.

import { afterEach, describe, expect, it, vi } from 'vitest';

it('stacks row splits when any pane would fall below its minimum width', () => {
  const [layout] = splitGroup(emptyLayout(), 'g1', 'right');
  const split = layout.root;
  if (split.type !== 'split') throw new Error('Expected a split');
  expect(minimumWidth(split)).toBe(640);
  expect(splitFits(split, 639)).toBe(false);
  expect(splitFits(split, 640)).toBe(true);
  expect(splitFits({ ...split, sizes: [0.2, 0.8] }, 1000)).toBe(false);
  expect(splitFits({ ...split, direction: 'column' }, 400)).toBe(true);
});
import {
  activate,
  activeGroupOf,
  closeGroup,
  closeOthers,
  closeTab,
  currentTab,
  DETAILS_WIDTH,
  dropTab,
  emptyLayout,
  focusGroup,
  groupsOf,
  keep,
  LIMITS,
  moveTab,
  moveTabToGroup,
  normalize,
  openTab,
  parseLayout,
  resizeSplit,
  reveal,
  setDetails,
  setView,
  shiftTab,
  splitGroup,
  stepGroup,
  minimumWidth,
  splitFits,
  stepTab,
  toStored,
  type Layout,
  type Split,
  type TabRef,
} from '../workbench/layout.ts';
import { createStore, loadLayout, saveLayout, STORAGE_PREFIX } from '../workbench/store.ts';

const chat = (session: string): TabRef => ({ kind: 'session', session, view: 'chat' });
const terminal = (session: string): TabRef => ({ kind: 'session', session, view: 'terminal' });
const file = (path: string): TabRef => ({ kind: 'file', workstream: 'WST1', location: 0, path });

/** Each pane's tabs as short labels (`S1:chat`, `a.ts`), a `*` marking the one on screen. */
function picture(layout: Layout): string[][] {
  return groupsOf(layout).map((g) =>
    g.tabs.map((t) => {
      const name = t.ref.kind === 'session' ? `${t.ref.session}:${t.ref.view}` : t.ref.path;
      return `${name}${t.preview ? '?' : ''}${t.id === g.active ? '*' : ''}`;
    }),
  );
}

const roundTrip = (layout: Layout) => parseLayout(JSON.parse(JSON.stringify(toStored(layout))));

/** The stored JSON, loosely typed: the tests reach in to damage one field at a time. */
interface StoredNode {
  type: string;
  id: string;
  active?: string | null;
  tabs: { id: string; ref: Record<string, unknown>; preview: unknown }[];
  children: StoredNode[];
  sizes: number[];
}
interface Stored {
  version: number;
  layout: { root: StoredNode; activeGroup: string; details: { open: boolean; width: number } };
}
const child = (node: StoredNode, index: number): StoredNode => {
  const found = node.children[index];
  if (found === undefined) throw new Error(`no child ${index}`);
  return found;
};
const tab = (node: StoredNode): StoredNode['tabs'][number] => {
  const found = node.tabs[0];
  if (found === undefined) throw new Error('no tab');
  return found;
};

describe('the layout model', () => {
  it('opens a session from the list as a preview that the next one replaces', () => {
    let layout = reveal(emptyLayout(), chat('S1'), { preview: true });
    expect(picture(layout)).toEqual([['S1:chat?*']]);
    layout = reveal(layout, chat('S2'), { preview: true });
    expect(picture(layout)).toEqual([['S2:chat?*']]);
    // Kept, it stays; the next preview opens beside it.
    const { group, tab } = currentTab(layout) ?? {};
    layout = keep(layout, group?.id ?? '', tab?.id ?? '');
    layout = reveal(layout, chat('S3'), { preview: true });
    expect(picture(layout)).toEqual([['S2:chat', 'S3:chat?*']]);
    // Opening what is open activates it; nothing new.
    const again = reveal(layout, chat('S2'), { preview: true });
    expect(picture(again)).toEqual([['S2:chat*', 'S3:chat?']]);
    expect(reveal(again, chat('S2'))).toBe(again);
  });

  it("switches an open tab to the view asked for, or finds the pane that shows it", () => {
    let layout = openTab(emptyLayout(), chat('S1'));
    layout = reveal(layout, terminal('S1'));
    expect(picture(layout)).toEqual([['S1:terminal*']]);

    // S1's chat on the left, its terminal on the right.
    layout = openTab(emptyLayout(), chat('S1'));
    const [split] = splitGroup(layout, activeGroupOf(layout).id, 'right');
    const right = activeGroupOf(split);
    layout = setView(split, right.id, right.active ?? '', 'terminal');
    const [left] = groupsOf(layout);
    layout = focusGroup(layout, left?.id ?? '');
    // Asking for the terminal goes to the pane showing it, without touching the chat.
    const shown = reveal(layout, terminal('S1'));
    expect(picture(shown)).toEqual([['S1:chat*'], ['S1:terminal*']]);
    expect(activeGroupOf(shown).id).toBe(right.id);
    // Asking for its work switches the session's tab in the active pane, the left.
    const work = reveal(layout, { kind: 'session', session: 'S1', view: 'work' });
    expect(picture(work)).toEqual([['S1:work*'], ['S1:terminal*']]);
  });

  it('opens kept tabs, beside the one on screen, and in a new pane to the side', () => {
    let layout = openTab(emptyLayout(), chat('S1'));
    layout = openTab(layout, file('a.ts'));
    layout = activate(layout, activeGroupOf(layout).id, activeGroupOf(layout).tabs[0]?.id ?? '');
    layout = openTab(layout, file('b.ts'));
    expect(picture(layout)).toEqual([['S1:chat', 'b.ts*', 'a.ts']]);
    expect(openTab(layout, file('b.ts'))).toBe(layout);

    layout = openTab(layout, terminal('S1'), { side: 'down' });
    expect(picture(layout)).toEqual([['S1:chat', 'b.ts*', 'a.ts'], ['S1:terminal*']]);
    expect((layout.root as Split).direction).toBe('column');
    expect(activeGroupOf(layout).tabs[0]?.ref).toEqual(terminal('S1'));
  });

  it('splits a pane with a copy of its tab, and merges splits of the same direction', () => {
    let layout = reveal(emptyLayout(), chat('S1'), { preview: true });
    const first = activeGroupOf(layout).id;
    const [split, created] = splitGroup(layout, first, 'right');
    layout = split;
    expect(picture(layout)).toEqual([['S1:chat*'], ['S1:chat*']]);
    expect(layout.activeGroup).toBe(created);
    expect((layout.root as Split).sizes).toEqual([0.5, 0.5]);
    [layout] = splitGroup(layout, first, 'right');
    const root = layout.root as Split;
    expect(root.children).toHaveLength(3);
    expect(root.sizes).toEqual([0.25, 0.25, 0.5]);
    [layout] = splitGroup(layout, first, 'down');
    expect(((layout.root as Split).children[0] as Split).direction).toBe('column');
    expect(groupsOf(layout)).toHaveLength(4);
    // A pane with nothing open splits into an empty pane.
    const [empty, blank] = splitGroup(emptyLayout(), 'g1', 'right');
    expect(picture(empty)).toEqual([[], []]);
    expect(blank).toBeDefined();
  });

  it('refuses more panes than the limit', () => {
    let layout = openTab(emptyLayout(), chat('S1'));
    for (let i = 0; i < LIMITS.groups + 4; i += 1) [layout] = splitGroup(layout, layout.activeGroup, i % 2 ? 'right' : 'down');
    expect(groupsOf(layout)).toHaveLength(LIMITS.groups);
    const [same, created] = splitGroup(layout, layout.activeGroup, 'right');
    expect(same).toBe(layout);
    expect(created).toBeUndefined();
  });

  it('closes tabs and panes; the only pane stays, empty', () => {
    let layout = openTab(emptyLayout(), file('a'));
    layout = openTab(layout, file('b'));
    layout = openTab(layout, file('c'));
    const group = activeGroupOf(layout);
    layout = activate(layout, group.id, group.tabs[1]?.id ?? '');
    layout = closeTab(layout, group.id, group.tabs[1]?.id ?? '');
    expect(picture(layout)).toEqual([['a', 'c*']]);
    layout = closeTab(layout, group.id, group.tabs[2]?.id ?? '');
    expect(picture(layout)).toEqual([['a*']]);

    const [split, right] = splitGroup(layout, group.id, 'right');
    expect(groupsOf(split)).toHaveLength(2);
    const closed = closeTab(split, right ?? '', activeGroupOf(split).active ?? '');
    expect(picture(closed)).toEqual([['a*']]);
    expect(closed.root.type).toBe('group');
    expect(closed.activeGroup).toBe(group.id);

    const only = closeTab(closed, group.id, group.tabs[0]?.id ?? '');
    expect(picture(only)).toEqual([[]]);
    expect(groupsOf(only)[0]?.active).toBeNull();
    expect(closeGroup(only, group.id)).toBe(only);
  });

  it('closes the other tabs of a pane', () => {
    let layout = openTab(emptyLayout(), file('a'));
    layout = openTab(layout, file('b'));
    layout = openTab(layout, file('c'));
    const group = activeGroupOf(layout);
    layout = closeOthers(layout, group.id, group.tabs[1]?.id ?? '');
    expect(picture(layout)).toEqual([['b*']]);
  });

  it('reorders tabs and moves them between panes; a moved tab comes on screen', () => {
    let layout = openTab(emptyLayout(), file('a'));
    layout = openTab(layout, file('b'));
    layout = openTab(layout, file('c'));
    const g = activeGroupOf(layout);
    const [a, , c] = g.tabs.map((t) => t.id) as [string, string, string];
    expect(picture(moveTab(layout, { group: g.id, tab: a }, { group: g.id, index: 3 }))).toEqual([['b', 'c', 'a*']]);
    expect(picture(moveTab(layout, { group: g.id, tab: c }, { group: g.id, index: 0 }))).toEqual([['c*', 'a', 'b']]);
    expect(picture(shiftTab(layout, g.id, a, 1))).toEqual([['b', 'a*', 'c']]);
    expect(picture(shiftTab(layout, g.id, c, -1))).toEqual([['a', 'c*', 'b']]);
    expect(shiftTab(layout, g.id, c, 1)).toBe(layout);

    const [split, right] = splitGroup(layout, g.id, 'right');
    // The copy (c) is there already; moving c across shows that one and drops the moved one.
    const merged = moveTab(split, { group: g.id, tab: c }, { group: right ?? '' });
    expect(picture(merged)).toEqual([['a', 'b*'], ['c*']]);
    const moved = moveTab(split, { group: g.id, tab: a }, { group: right ?? '', index: 0 });
    expect(picture(moved)).toEqual([['b', 'c*'], ['a*', 'c']]);
    expect(moved.activeGroup).toBe(right);
    // The last tab out of a pane takes the pane with it.
    const [, , second] = groupsOf(split);
    expect(second).toBeUndefined();
    const emptied = moveTab(merged, { group: right ?? '', tab: activeGroupOf(merged).tabs[0]?.id ?? '' }, { group: g.id });
    expect(picture(emptied)).toEqual([['a', 'b', 'c*']]);
    expect(emptied.root.type).toBe('group');
  });

  it('drops a tab into a pane or into a new pane on any side', () => {
    let layout = openTab(emptyLayout(), file('a'));
    layout = openTab(layout, file('b'));
    const g = activeGroupOf(layout);
    const b = g.tabs[1]?.id ?? '';
    const sides = {
      left: [['b*'], ['a*']],
      right: [['a*'], ['b*']],
      top: [['b*'], ['a*']],
      bottom: [['a*'], ['b*']],
    } as const;
    for (const [side, expected] of Object.entries(sides)) {
      const dropped = dropTab(layout, { group: g.id, tab: b }, g.id, side as keyof typeof sides);
      expect(picture(dropped), side).toEqual(expected);
      expect((dropped.root as Split).direction, side).toBe(side === 'left' || side === 'right' ? 'row' : 'column');
    }
    expect(dropTab(layout, { group: g.id, tab: b }, g.id, 'center')).toBe(layout);
    // A preview left behind stays a preview: only a copy keeps its source.
    const withPreview = reveal(openTab(emptyLayout(), file('kept')), chat('S9'), { preview: true });
    const moved = dropTab(withPreview, { group: 'g1', tab: activeGroupOf(withPreview).tabs[0]?.id ?? '' }, 'g1', 'right');
    expect(picture(moved)).toEqual([['S9:chat?*'], ['kept*']]);
    // A pane's only tab dropped on its own side changes nothing.
    const single = openTab(emptyLayout(), file('a'));
    expect(dropTab(single, { group: 'g1', tab: activeGroupOf(single).active ?? '' }, 'g1', 'right')).toBe(single);
  });

  it('steps through tabs and panes, wrapping around, and moves a tab to the next pane', () => {
    let layout = openTab(emptyLayout(), file('a'));
    layout = openTab(layout, file('b'));
    expect(picture(stepTab(layout, 1))).toEqual([['a*', 'b']]);
    expect(picture(stepTab(layout, -1))).toEqual([['a*', 'b']]);
    const [split] = splitGroup(layout, layout.activeGroup, 'right');
    const [left, right] = groupsOf(split);
    expect(stepGroup(split, 1).activeGroup).toBe(left?.id);
    expect(stepGroup(split, -1).activeGroup).toBe(left?.id);
    expect(stepGroup(stepGroup(split, 1), 1).activeGroup).toBe(right?.id);

    // With one pane, moving the tab on screen makes a pane for it.
    const moved = moveTabToGroup(layout, 1);
    expect(picture(moved)).toEqual([['a*'], ['b*']]);
    expect(picture(moveTabToGroup(moved, 1))).toEqual([['a', 'b*']]);
  });

  it('resizes a split, keeping both sides at least the minimum', () => {
    let layout = openTab(emptyLayout(), file('a'));
    [layout] = splitGroup(layout, layout.activeGroup, 'right');
    const id = layout.root.id;
    const sizes = (l: Layout) => (l.root as Split).sizes.map((s) => Math.round(s * 100) / 100);
    expect(sizes(resizeSplit(layout, id, 0, 0.2))).toEqual([0.7, 0.3]);
    expect(sizes(resizeSplit(layout, id, 0, -2))).toEqual([LIMITS.minSize, 1 - LIMITS.minSize]);
    expect(resizeSplit(layout, id, 0, Number.NaN)).toBe(layout);
    expect(resizeSplit(layout, 'nope', 0, 0.1)).toBe(layout);
  });

  it('keeps the details sidebar in range', () => {
    const layout = setDetails(emptyLayout(), { open: true, width: 10_000 });
    expect(layout.details).toEqual({ open: true, width: DETAILS_WIDTH.max });
    expect(setDetails(layout, { open: true })).toBe(layout);
  });

  it('tidies sizes that do not sum to one', () => {
    let layout = openTab(emptyLayout(), file('a'));
    [layout] = splitGroup(layout, layout.activeGroup, 'right');
    const skewed = { ...layout, root: { ...(layout.root as Split), sizes: [3, 1] } };
    expect((normalize(skewed).root as Split).sizes).toEqual([0.75, 0.25]);
  });
});

describe('storing the layout', () => {
  const complex = () => {
    let layout = reveal(emptyLayout(), chat('S1'), { preview: true });
    layout = openTab(layout, file('src/a.ts'), { side: 'right' });
    layout = openTab(layout, terminal('S2'), { side: 'down' });
    layout = setDetails(layout, { open: true, width: 333 });
    return resizeSplit(layout, layout.root.id, 0, 0.1);
  };

  it('reads back what it stored', () => {
    const layout = complex();
    expect(roundTrip(layout)).toEqual(layout);
    // New ids never clash with stored ones.
    const read = roundTrip(layout) as Layout;
    const opened = openTab(read, file('new'));
    const ids = groupsOf(opened).flatMap((g) => [g.id, ...g.tabs.map((t) => t.id)]);
    expect(new Set(ids).size).toBe(ids.length);
  });

  it('refuses anything malformed, as a whole', () => {
    const good = JSON.parse(JSON.stringify(toStored(complex()))) as Stored;
    const broken: [string, (stored: Stored) => unknown][] = [
      ['not an object', () => 'layout'],
      ['null', () => null],
      ['another version', (s) => ({ ...s, version: 2 })],
      ['no layout', (s) => ({ version: s.version })],
      ['no root', (s) => ({ ...s, layout: { ...s.layout, root: undefined } })],
      ['an unknown node', (s) => ({ ...s, layout: { ...s.layout, root: { type: 'window', id: 'g1' } } })],
      ['a duplicate id', (s) => {
        child(child(s.layout.root, 1), 0).id = child(s.layout.root, 0).id;
        return s;
      }],
      ['a malformed id', (s) => {
        s.layout.root.id = 'root';
        return s;
      }],
      ['an active tab that is not there', (s) => {
        child(s.layout.root, 0).active = 't999';
        return s;
      }],
      ['an unknown view', (s) => {
        tab(child(s.layout.root, 0)).ref.view = 'video';
        return s;
      }],
      ['a negative location', (s) => {
        tab(child(child(s.layout.root, 1), 0)).ref.location = -1;
        return s;
      }],
      ['an empty path', (s) => {
        tab(child(child(s.layout.root, 1), 0)).ref.path = '';
        return s;
      }],
      ['a size of zero', (s) => {
        s.layout.root.sizes = [0, 1];
        return s;
      }],
      ['sizes that do not match', (s) => {
        s.layout.root.sizes = [1];
        return s;
      }],
      ['a split of one', (s) => {
        s.layout.root.children = [child(s.layout.root, 0)];
        s.layout.root.sizes = [1];
        return s;
      }],
      ['a preview that is not a boolean', (s) => {
        tab(child(s.layout.root, 0)).preview = 'yes';
        return s;
      }],
      ['no details', (s) => ({ ...s, layout: { ...s.layout, details: undefined } })],
      ['too many tabs', (s) => {
        const first = child(s.layout.root, 0);
        first.tabs = Array.from({ length: LIMITS.tabs + 1 }, (_, i) => ({
          id: `t${100 + i}`,
          ref: { kind: 'session', session: `S${i}`, view: 'chat' },
          preview: false,
        }));
        first.active = 't100';
        return s;
      }],
      ['nesting too deep', (s) => {
        let node: unknown = { type: 'group', id: 'g500', tabs: [], active: null };
        for (let i = 0; i < LIMITS.depth + 2; i += 1) {
          const sibling = { type: 'group', id: `g${700 + i}`, tabs: [], active: null };
          node = { type: 'split', id: `s${600 + i}`, direction: i % 2 ? 'row' : 'column', children: [node, sibling], sizes: [1, 1] };
        }
        return { ...s, layout: { ...s.layout, root: node } };
      }],
    ];
    for (const [name, damage] of broken) {
      expect(parseLayout(damage(JSON.parse(JSON.stringify(good)))), name).toBeUndefined();
    }
    // An active pane that is gone falls back to the first; a too-wide sidebar is brought into range.
    const drifted = JSON.parse(JSON.stringify(good)) as Stored;
    drifted.layout.activeGroup = 'g999';
    drifted.layout.details.width = 99_999;
    const read = parseLayout(drifted) as Layout;
    expect(read.activeGroup).toBe(groupsOf(read)[0]?.id);
    expect(read.details.width).toBe(DETAILS_WIDTH.max);
  });

  afterEach(() => {
    localStorage.clear();
    vi.restoreAllMocks();
  });

  it('falls back to the empty layout when storage is missing, corrupt or refuses', () => {
    expect(loadLayout('WS1')).toEqual(emptyLayout());
    for (const raw of ['{', 'null', '42', '"text"', '[]', '{"version":1}', JSON.stringify({ version: 1, layout: {} })]) {
      localStorage.setItem(`${STORAGE_PREFIX}WS1`, raw);
      expect(loadLayout('WS1'), raw).toEqual(emptyLayout());
    }
    vi.spyOn(Storage.prototype, 'getItem').mockImplementation(() => {
      throw new Error('SecurityError');
    });
    expect(loadLayout('WS1')).toEqual(emptyLayout());
    vi.spyOn(Storage.prototype, 'setItem').mockImplementation(() => {
      throw new Error('QuotaExceededError');
    });
    expect(() => saveLayout('WS1', complex())).not.toThrow();
  });

  it('keeps one layout per workspace, saving each change', () => {
    const one = createStore('WS1');
    const two = createStore('WS2');
    const heard = vi.fn();
    const off = one.subscribe(heard);
    one.update((layout) => openTab(layout, file('a')));
    expect(heard).toHaveBeenCalledTimes(1);
    // A change that changes nothing is not saved or heard.
    one.update((layout) => layout);
    expect(heard).toHaveBeenCalledTimes(1);
    off();
    expect(picture(two.get())).toEqual([[]]);
    expect(picture(createStore('WS1').get())).toEqual([['a*']]);
    expect(localStorage.getItem(`${STORAGE_PREFIX}WS2`)).toBeNull();
  });
});
