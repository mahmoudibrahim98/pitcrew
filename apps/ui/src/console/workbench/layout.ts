// The workbench's layout: panes (groups of tabs) in nested splits, as an IDE's editor area. Pure
// data and functions, no React: every change returns a new layout (or the same object when nothing
// changed), so the store can tell whether to save and notify.
//
// - A tab shows a session (its chat, terminal or work) or a file of a workstream's folder.
// - A pane holds tabs, one of them on screen. Panes sit in splits, side by side (`row`) or one
//   above the other (`column`), each with its share of the split (`sizes`, summing to 1).
// - One pane is active: where the list and the palette open things.
// - A session chosen from the list opens as the pane's **preview** tab, which the next one
//   replaces, so following the list with the arrow keys does not pile up tabs. A preview becomes a
//   kept tab when the person keeps it (double-click, "Keep open"), switches its view, moves it or
//   splits it.

export type SessionView = 'chat' | 'terminal' | 'work';
export const SESSION_VIEWS: readonly SessionView[] = ['chat', 'terminal', 'work'];

export type TabRef =
  | { kind: 'session'; session: string; view: SessionView }
  | { kind: 'file'; workstream: string; location: number; path: string };

export interface Tab {
  id: string;
  ref: TabRef;
  /** Replaced by the next session opened from the list into this pane. */
  preview: boolean;
}

export interface Group {
  type: 'group';
  id: string;
  tabs: Tab[];
  /** The tab on screen; null only when the pane is empty. */
  active: string | null;
}

export interface Split {
  type: 'split';
  id: string;
  /** `row`: side by side; `column`: one above the other. */
  direction: 'row' | 'column';
  children: LayoutNode[];
  /** Each child's share, in order; they sum to 1. */
  sizes: number[];
}

export type LayoutNode = Group | Split;

export interface Details {
  open: boolean;
  width: number;
}

export interface Layout {
  root: LayoutNode;
  activeGroup: string;
  details: Details;
  /** The next number for a new id. */
  seq: number;
}

/** Where a tab dropped on a pane goes: into it, or into a new pane on one of its sides. */
export type DropSide = 'center' | 'left' | 'right' | 'top' | 'bottom';

export const LIMITS = {
  groups: 16,
  tabs: 100,
  depth: 8,
  /** No pane gets less than this share of its split. */
  minSize: 0.1,
  pathLength: 4096,
  idLength: 128,
} as const;

export const DETAILS_WIDTH = { min: 240, max: 520, initial: 300 } as const;

// ─── Reading ────────────────────────────────────────────────────────────────────────────────────

export function emptyLayout(): Layout {
  return {
    root: { type: 'group', id: 'g1', tabs: [], active: null },
    activeGroup: 'g1',
    details: { open: false, width: DETAILS_WIDTH.initial },
    seq: 2,
  };
}

/** Every pane, in reading order (left to right, top to bottom). */
export function groupsOf(layout: Layout): Group[] {
  const out: Group[] = [];
  const walk = (node: LayoutNode) => {
    if (node.type === 'group') out.push(node);
    else node.children.forEach(walk);
  };
  walk(layout.root);
  return out;
}

export function findGroup(layout: Layout, id: string): Group | undefined {
  return groupsOf(layout).find((g) => g.id === id);
}

export function activeGroupOf(layout: Layout): Group {
  const groups = groupsOf(layout);
  return groups.find((g) => g.id === layout.activeGroup) ?? (groups[0] as Group);
}

export function activeTabOf(group: Group): Tab | undefined {
  return group.tabs.find((t) => t.id === group.active);
}

/** The tab on screen in the active pane. */
export function currentTab(layout: Layout): { group: Group; tab: Tab } | undefined {
  const group = activeGroupOf(layout);
  const tab = activeTabOf(group);
  return tab === undefined ? undefined : { group, tab };
}

export function tabCount(layout: Layout): number {
  return groupsOf(layout).reduce((sum, g) => sum + g.tabs.length, 0);
}

/** The same thing on screen: the same session in the same view, or the same file. */
export function sameRef(a: TabRef, b: TabRef): boolean {
  if (a.kind === 'session' && b.kind === 'session') return a.session === b.session && a.view === b.view;
  if (a.kind === 'file' && b.kind === 'file') {
    return a.workstream === b.workstream && a.location === b.location && a.path === b.path;
  }
  return false;
}

const sameSession = (a: TabRef, b: TabRef) => a.kind === 'session' && b.kind === 'session' && a.session === b.session;

// ─── Building blocks ────────────────────────────────────────────────────────────────────────────

function nextId(layout: Layout, prefix: 'g' | 's' | 't'): [string, Layout] {
  return [`${prefix}${layout.seq}`, { ...layout, seq: layout.seq + 1 }];
}

/** `node` with the group `id` replaced by `replace(group)` (a group, a split, or null to drop it). */
function mapGroup(node: LayoutNode, id: string, replace: (group: Group) => LayoutNode | null): LayoutNode | null {
  if (node.type === 'group') return node.id === id ? replace(node) : node;
  let changed = false;
  const children: LayoutNode[] = [];
  const sizes: number[] = [];
  node.children.forEach((child, i) => {
    const next = mapGroup(child, id, replace);
    if (next !== child) changed = true;
    if (next !== null) {
      children.push(next);
      sizes.push(node.sizes[i] ?? 1 / node.children.length);
    }
  });
  if (!changed) return node;
  if (children.length === 0) return null;
  return { ...node, children, sizes };
}

function updateGroup(layout: Layout, id: string, update: (group: Group) => Group): Layout {
  const root = mapGroup(layout.root, id, update);
  return root === layout.root || root === null ? layout : normalize({ ...layout, root });
}

const sum = (values: readonly number[]) => values.reduce((a, b) => a + b, 0);

/**
 * Tidies a layout after a change: a split of one is its child; a split inside a split of the same
 * direction is merged into it; sizes are positive and sum to 1; the active pane and each pane's
 * active tab exist. Empty panes stay (closing a pane's last tab removes the pane itself).
 */
export function normalize(layout: Layout): Layout {
  const tidy = (node: LayoutNode, isRoot: boolean): LayoutNode | null => {
    if (node.type === 'group') {
      const active = node.tabs.some((t) => t.id === node.active) ? node.active : (node.tabs[0]?.id ?? null);
      return active === node.active ? node : { ...node, active };
    }
    const children: LayoutNode[] = [];
    const sizes: number[] = [];
    node.children.forEach((child, i) => {
      const next = tidy(child, false);
      if (next === null) return;
      const raw = node.sizes[i];
      const size = typeof raw === 'number' && Number.isFinite(raw) && raw > 0 ? raw : 1 / node.children.length;
      if (next.type === 'split' && next.direction === node.direction) {
        const inner = sum(next.sizes);
        next.children.forEach((grandchild, j) => {
          children.push(grandchild);
          sizes.push(size * ((next.sizes[j] ?? 0) / inner));
        });
      } else {
        children.push(next);
        sizes.push(size);
      }
    });
    if (children.length === 0) return isRoot ? { type: 'group', id: node.id.replace(/^s/, 'g'), tabs: [], active: null } : null;
    if (children.length === 1) return children[0] as LayoutNode;
    const total = sum(sizes);
    return { ...node, children, sizes: sizes.map((s) => s / total) };
  };
  const root = tidy(layout.root, true) as LayoutNode;
  const groups: Group[] = [];
  const walk = (node: LayoutNode) => (node.type === 'group' ? groups.push(node) : node.children.forEach(walk));
  walk(root);
  const activeGroup = groups.some((g) => g.id === layout.activeGroup) ? layout.activeGroup : (groups[0] as Group).id;
  return { ...layout, root, activeGroup };
}

// ─── Opening ────────────────────────────────────────────────────────────────────────────────────

function activateIn(layout: Layout, groupId: string, tabId: string, change?: (tab: Tab) => Tab): Layout {
  const group = findGroup(layout, groupId);
  if (group === undefined) return layout;
  const tab = group.tabs.find((t) => t.id === tabId);
  if (tab === undefined) return layout;
  const next = change?.(tab) ?? tab;
  const unchanged = next === tab && group.active === tabId && layout.activeGroup === groupId;
  if (unchanged) return layout;
  const updated = updateGroup(layout, groupId, (g) => ({
    ...g,
    active: tabId,
    tabs: next === tab ? g.tabs : g.tabs.map((t) => (t.id === tabId ? next : t)),
  }));
  return updated.activeGroup === groupId ? updated : { ...updated, activeGroup: groupId };
}

const withView = (view: SessionView) => (tab: Tab): Tab =>
  tab.ref.kind === 'session' && tab.ref.view !== view ? { ...tab, ref: { ...tab.ref, view } } : tab;

/**
 * Shows `ref`, as following a link does: activates it where it is already open (in the active
 * pane first), or switches an open tab of the same session to `ref`'s view, or opens it in the
 * active pane. Opened from the list (`preview`), it replaces the pane's preview tab. With
 * `anyView`, a session open in any view is shown as it is (choosing a session in the list leaves
 * the view each of its tabs has).
 */
export function reveal(layout: Layout, ref: TabRef, options: { preview?: boolean; anyView?: boolean } = {}): Layout {
  const active = activeGroupOf(layout);
  const others = groupsOf(layout).filter((g) => g !== active);
  const matchers = options.anyView === true
    ? [(t: Tab) => sameSession(t.ref, ref) || sameRef(t.ref, ref)]
    : [(t: Tab) => sameRef(t.ref, ref), (t: Tab) => sameSession(t.ref, ref)];
  for (const match of matchers) {
    for (const group of [active, ...others]) {
      // The active tab first, so a pane showing the session keeps showing it.
      const current = activeTabOf(group);
      const tab = current !== undefined && match(current) ? current : group.tabs.find(match);
      if (tab !== undefined) {
        const switchView = ref.kind === 'session' && options.anyView !== true ? withView(ref.view) : undefined;
        return activateIn(layout, group.id, tab.id, switchView);
      }
    }
  }
  return openIn(layout, active.id, ref, options.preview === true);
}

/** Opens `ref` as a new tab in pane `groupId` (a preview replaces the pane's preview tab). */
function openIn(layout: Layout, groupId: string, ref: TabRef, preview: boolean): Layout {
  const group = findGroup(layout, groupId);
  if (group === undefined) return layout;
  const [id, next] = nextId(layout, 't');
  const tab: Tab = { id, ref, preview };
  const replaced = preview ? group.tabs.findIndex((t) => t.preview) : -1;
  let tabs: Tab[];
  if (replaced !== -1) {
    tabs = group.tabs.map((t, i) => (i === replaced ? tab : t));
  } else {
    if (group.tabs.length >= LIMITS.tabs) return layout;
    const at = group.tabs.findIndex((t) => t.id === group.active);
    tabs = [...group.tabs.slice(0, at + 1), tab, ...group.tabs.slice(at + 1)];
  }
  const updated = updateGroup(next, groupId, (g) => ({ ...g, tabs, active: id }));
  return { ...updated, activeGroup: groupId };
}

/**
 * Opens `ref` as a kept tab: in pane `group` (the active pane by default), activating it if that
 * pane already has it; with `side`, in a new pane split off to the right or below.
 */
export function openTab(
  layout: Layout,
  ref: TabRef,
  options: { group?: string; side?: 'right' | 'down' } = {},
): Layout {
  const groupId = options.group ?? activeGroupOf(layout).id;
  if (options.side !== undefined) {
    const [split, newGroup] = splitGroup(layout, groupId, options.side, { copy: false });
    if (newGroup === undefined) return layout;
    return openIn(split, newGroup, ref, false);
  }
  const group = findGroup(layout, groupId);
  if (group === undefined) return layout;
  const existing = group.tabs.find((t) => sameRef(t.ref, ref));
  if (existing !== undefined) return activateIn(layout, groupId, existing.id, keepTab);
  return openIn(layout, groupId, ref, false);
}

const keepTab = (tab: Tab): Tab => (tab.preview ? { ...tab, preview: false } : tab);

// ─── Tabs ───────────────────────────────────────────────────────────────────────────────────────

export function activate(layout: Layout, groupId: string, tabId: string): Layout {
  return activateIn(layout, groupId, tabId);
}

/** Turns a preview tab into a kept one. */
export function keep(layout: Layout, groupId: string, tabId: string): Layout {
  return activateIn(layout, groupId, tabId, keepTab);
}

/** Switches a session tab's view (which keeps it). */
export function setView(layout: Layout, groupId: string, tabId: string, view: SessionView): Layout {
  return activateIn(layout, groupId, tabId, (tab) => keepTab(withView(view)(tab)));
}

/**
 * Closes a tab. The tab after it comes on screen (or the one before, at the end); a pane left
 * empty goes, unless it is the only one.
 */
export function closeTab(layout: Layout, groupId: string, tabId: string): Layout {
  const group = findGroup(layout, groupId);
  const index = group?.tabs.findIndex((t) => t.id === tabId) ?? -1;
  if (group === undefined || index === -1) return layout;
  const tabs = group.tabs.filter((t) => t.id !== tabId);
  if (tabs.length === 0) return closeGroup(layout, groupId);
  const active = group.active === tabId ? (tabs[Math.min(index, tabs.length - 1)] as Tab).id : group.active;
  return updateGroup(layout, groupId, (g) => ({ ...g, tabs, active }));
}

/**
 * Closes a pane with its tabs. The only pane is emptied instead. When the active pane goes, the
 * one before it (or after) becomes active.
 */
export function closeGroup(layout: Layout, groupId: string): Layout {
  const groups = groupsOf(layout);
  const at = groups.findIndex((g) => g.id === groupId);
  if (at === -1) return layout;
  if (groups.length === 1) return updateGroup(layout, groupId, (g) => (g.tabs.length === 0 ? g : { ...g, tabs: [], active: null }));
  const root = mapGroup(layout.root, groupId, () => null);
  if (root === null) return layout;
  const next = normalize({ ...layout, root });
  if (layout.activeGroup !== groupId) return next;
  const neighbour = (groups[at - 1] ?? groups[at + 1]) as Group;
  return { ...next, activeGroup: neighbour.id };
}

/** Closes every tab of the pane but `tabId`. */
export function closeOthers(layout: Layout, groupId: string, tabId: string): Layout {
  const group = findGroup(layout, groupId);
  if (group === undefined || !group.tabs.some((t) => t.id === tabId)) return layout;
  if (group.tabs.length === 1) return activateIn(layout, groupId, tabId);
  return activateIn(
    updateGroup(layout, groupId, (g) => ({ ...g, tabs: g.tabs.filter((t) => t.id === tabId), active: tabId })),
    groupId,
    tabId,
  );
}

/** The next (`step` 1) or previous (-1) tab of the active pane, wrapping around. */
export function stepTab(layout: Layout, step: 1 | -1): Layout {
  const group = activeGroupOf(layout);
  if (group.tabs.length < 2) return layout;
  const at = group.tabs.findIndex((t) => t.id === group.active);
  const next = group.tabs[(at + step + group.tabs.length) % group.tabs.length] as Tab;
  return activateIn(layout, group.id, next.id);
}

/** The next or previous pane becomes the active one, wrapping around. */
export function stepGroup(layout: Layout, step: 1 | -1): Layout {
  const groups = groupsOf(layout);
  if (groups.length < 2) return layout;
  const at = groups.findIndex((g) => g.id === layout.activeGroup);
  return focusGroup(layout, (groups[(at + step + groups.length) % groups.length] as Group).id);
}

export function focusGroup(layout: Layout, groupId: string): Layout {
  if (layout.activeGroup === groupId || findGroup(layout, groupId) === undefined) return layout;
  return { ...layout, activeGroup: groupId };
}

// ─── Splitting and moving ───────────────────────────────────────────────────────────────────────

/**
 * Splits pane `groupId`: a new pane to its right (`right`, `left`) or below it (`down`, `up`), or
 * before it for `left` and `up`. With `copy`, the new pane opens a copy of the pane's tab on screen
 * (as an editor's "split" does); otherwise it starts empty. Returns the layout and the new pane's
 * id (undefined when nothing could be split: too many panes, or no such pane).
 */
export function splitGroup(
  layout: Layout,
  groupId: string,
  side: 'right' | 'down' | 'left' | 'up',
  options: { copy?: boolean } = {},
): [Layout, string | undefined] {
  const group = findGroup(layout, groupId);
  if (group === undefined || groupsOf(layout).length >= LIMITS.groups) return [layout, undefined];
  const [id, withGroup] = nextId(layout, 'g');
  let next = withGroup;
  let tabs: Tab[] = [];
  const source = activeTabOf(group);
  if (options.copy !== false && source !== undefined) {
    const [tabId, withTab] = nextId(next, 't');
    next = withTab;
    tabs = [{ id: tabId, ref: source.ref, preview: false }];
  }
  const created: Group = { type: 'group', id, tabs, active: tabs[0]?.id ?? null };
  const [splitId, withSplit] = nextId(next, 's');
  next = withSplit;
  const direction = side === 'right' || side === 'left' ? 'row' : 'column';
  const before = side === 'left' || side === 'up';
  const root = mapGroup(next.root, groupId, (g) => ({
    type: 'split',
    id: splitId,
    direction,
    children: before ? [created, g] : [g, created],
    sizes: [0.5, 0.5],
  }));
  if (root === null) return [layout, undefined];
  // A tab that was copied is kept on both sides: the source is no longer a preview either.
  const kept =
    tabs.length === 0
      ? root
      : mapGroup(root, groupId, (g) => ({ ...g, tabs: g.tabs.map((t) => (t.id === source?.id ? keepTab(t) : t)) }));
  return [normalize({ ...next, root: kept ?? root, activeGroup: id }), id];
}

/**
 * Moves a tab to pane `to`, at `index` (the end by default): to reorder it within its pane, or to
 * drag it to another. It arrives kept and on screen, and its pane becomes active. Where the target
 * pane already shows the same thing, that tab comes on screen instead and the moved one goes. A
 * pane left empty goes.
 */
export function moveTab(layout: Layout, from: { group: string; tab: string }, to: { group: string; index?: number }): Layout {
  const source = findGroup(layout, from.group);
  const target = findGroup(layout, to.group);
  const tab = source?.tabs.find((t) => t.id === from.tab);
  if (source === undefined || target === undefined || tab === undefined) return layout;
  const moved = keepTab(tab);
  if (source === target) {
    const without = source.tabs.filter((t) => t.id !== tab.id);
    const from_ = source.tabs.indexOf(tab);
    let index = Math.max(0, Math.min(to.index ?? without.length, source.tabs.length));
    // An index past the tab's own place counts the tab itself.
    if (index > from_) index -= 1;
    const tabs = [...without.slice(0, index), moved, ...without.slice(index)];
    if (tabs.every((t, i) => t === source.tabs[i]) && source.active === tab.id && layout.activeGroup === source.id) {
      return layout;
    }
    return { ...updateGroup(layout, source.id, (g) => ({ ...g, tabs, active: tab.id })), activeGroup: source.id };
  }
  const duplicate = target.tabs.find((t) => sameRef(t.ref, tab.ref));
  let next = closeTab(layout, source.id, tab.id);
  if (duplicate !== undefined) return activateIn(next, target.id, duplicate.id, keepTab);
  if (findGroup(next, target.id) === undefined) return layout;
  next = updateGroup(next, target.id, (g) => {
    const index = Math.max(0, Math.min(to.index ?? g.tabs.length, g.tabs.length));
    return { ...g, tabs: [...g.tabs.slice(0, index), moved, ...g.tabs.slice(index)], active: moved.id };
  });
  return { ...next, activeGroup: target.id };
}

/**
 * Drops a tab on pane `targetGroup`: into it (`center`, at the end), or into a new pane split off
 * on that side. Moving a pane's only tab to one of its own sides changes nothing.
 */
export function dropTab(layout: Layout, from: { group: string; tab: string }, targetGroup: string, side: DropSide): Layout {
  if (side === 'center') return from.group === targetGroup ? layout : moveTab(layout, from, { group: targetGroup });
  const source = findGroup(layout, from.group);
  if (source === undefined || !source.tabs.some((t) => t.id === from.tab)) return layout;
  if (source.id === targetGroup && source.tabs.length === 1) return layout;
  const where = side === 'top' ? 'up' : side === 'bottom' ? 'down' : side;
  const [split, created] = splitGroup(layout, targetGroup, where, { copy: false });
  if (created === undefined) return layout;
  return moveTab(split, from, { group: created });
}

/**
 * Moves the active tab into the next (or previous) pane; with one pane, into a new pane on the
 * right (or left).
 */
export function moveTabToGroup(layout: Layout, step: 1 | -1): Layout {
  const current = currentTab(layout);
  if (current === undefined) return layout;
  const groups = groupsOf(layout);
  if (groups.length === 1) {
    return dropTab(layout, { group: current.group.id, tab: current.tab.id }, current.group.id, step === 1 ? 'right' : 'left');
  }
  const at = groups.indexOf(current.group);
  const target = groups[(at + step + groups.length) % groups.length] as Group;
  return moveTab(layout, { group: current.group.id, tab: current.tab.id }, { group: target.id });
}

/** Moves the active tab one place left (-1) or right (1) within its pane. */
export function shiftTab(layout: Layout, groupId: string, tabId: string, step: 1 | -1): Layout {
  const group = findGroup(layout, groupId);
  const at = group?.tabs.findIndex((t) => t.id === tabId) ?? -1;
  if (group === undefined || at === -1) return layout;
  const index = at + step;
  if (index < 0 || index >= group.tabs.length) return layout;
  return moveTab(layout, { group: groupId, tab: tabId }, { group: groupId, index: step === 1 ? index + 1 : index });
}

/**
 * Moves the boundary between child `index` and `index + 1` of split `splitId` by `delta` (a share
 * of the split), keeping both sides at least `LIMITS.minSize`.
 */
export function resizeSplit(layout: Layout, splitId: string, index: number, delta: number): Layout {
  let changed = false;
  const walk = (node: LayoutNode): LayoutNode => {
    if (node.type === 'group') return node;
    if (node.id !== splitId) {
      const children = node.children.map(walk);
      return children.every((c, i) => c === node.children[i]) ? node : { ...node, children };
    }
    const a = node.sizes[index];
    const b = node.sizes[index + 1];
    if (a === undefined || b === undefined || !Number.isFinite(delta)) return node;
    const pair = a + b;
    const nextA = Math.min(pair - LIMITS.minSize, Math.max(LIMITS.minSize, a + delta));
    if (Math.abs(nextA - a) < 1e-9) return node;
    changed = true;
    const sizes = [...node.sizes];
    sizes[index] = nextA;
    sizes[index + 1] = pair - nextA;
    return { ...node, sizes };
  };
  const root = walk(layout.root);
  return changed ? { ...layout, root } : layout;
}

export function setDetails(layout: Layout, details: Partial<Details>): Layout {
  const open = details.open ?? layout.details.open;
  const width = clampWidth(details.width ?? layout.details.width);
  if (open === layout.details.open && width === layout.details.width) return layout;
  return { ...layout, details: { open, width } };
}

const clampWidth = (width: number) =>
  Number.isFinite(width) ? Math.round(Math.min(DETAILS_WIDTH.max, Math.max(DETAILS_WIDTH.min, width))) : DETAILS_WIDTH.initial;

// ─── Storing ────────────────────────────────────────────────────────────────────────────────────

export const STORED_VERSION = 1;

/** What goes into storage: the layout as it is, with a version. */
export function toStored(layout: Layout): { version: number; layout: Layout } {
  return { version: STORED_VERSION, layout };
}

const isRecord = (value: unknown): value is Record<string, unknown> =>
  typeof value === 'object' && value !== null && !Array.isArray(value);
const isText = (value: unknown, max: number): value is string =>
  typeof value === 'string' && value.length > 0 && value.length <= max;

function parseRef(raw: unknown): TabRef | undefined {
  if (!isRecord(raw)) return undefined;
  if (raw.kind === 'session') {
    const view = raw.view;
    if (!isText(raw.session, LIMITS.idLength) || !SESSION_VIEWS.includes(view as SessionView)) return undefined;
    return { kind: 'session', session: raw.session, view: view as SessionView };
  }
  if (raw.kind === 'file') {
    const { workstream, location, path } = raw;
    if (!isText(workstream, LIMITS.idLength) || !isText(path, LIMITS.pathLength)) return undefined;
    if (typeof location !== 'number' || !Number.isSafeInteger(location) || location < 0) return undefined;
    return { kind: 'file', workstream, location, path };
  }
  return undefined;
}

/**
 * A stored layout, checked: anything missing, malformed, out of range or too large gives
 * undefined (the caller starts from `emptyLayout()`), never a half-read layout.
 */
export function parseLayout(raw: unknown): Layout | undefined {
  if (!isRecord(raw) || raw.version !== STORED_VERSION || !isRecord(raw.layout)) return undefined;
  const stored = raw.layout;
  const ids = new Set<string>();
  let groups = 0;
  let highest = 0;
  const claim = (id: unknown, prefix: string): id is string => {
    if (typeof id !== 'string' || !new RegExp(`^${prefix}[1-9][0-9]{0,8}$`).test(id) || ids.has(id)) return false;
    ids.add(id);
    highest = Math.max(highest, Number(id.slice(1)));
    return true;
  };
  const node = (value: unknown, depth: number): LayoutNode | undefined => {
    if (!isRecord(value) || depth > LIMITS.depth) return undefined;
    if (value.type === 'group') {
      if (!claim(value.id, 'g') || !Array.isArray(value.tabs) || value.tabs.length > LIMITS.tabs) return undefined;
      if ((groups += 1) > LIMITS.groups) return undefined;
      const tabs: Tab[] = [];
      for (const rawTab of value.tabs) {
        if (!isRecord(rawTab) || !claim(rawTab.id, 't') || typeof rawTab.preview !== 'boolean') return undefined;
        const ref = parseRef(rawTab.ref);
        if (ref === undefined) return undefined;
        tabs.push({ id: rawTab.id, ref, preview: rawTab.preview });
      }
      const active = value.active;
      if (active !== null && !(typeof active === 'string' && tabs.some((t) => t.id === active))) return undefined;
      return { type: 'group', id: value.id, tabs, active };
    }
    if (value.type === 'split') {
      const { children, sizes, direction } = value;
      if (!claim(value.id, 's') || (direction !== 'row' && direction !== 'column')) return undefined;
      if (!Array.isArray(children) || !Array.isArray(sizes) || children.length < 2 || sizes.length !== children.length) {
        return undefined;
      }
      if (!sizes.every((s) => typeof s === 'number' && Number.isFinite(s) && s > 0)) return undefined;
      const parsed: LayoutNode[] = [];
      for (const child of children) {
        const next = node(child, depth + 1);
        if (next === undefined) return undefined;
        parsed.push(next);
      }
      return { type: 'split', id: value.id, direction, children: parsed, sizes: sizes as number[] };
    }
    return undefined;
  };
  const root = node(stored.root, 0);
  if (root === undefined || typeof stored.activeGroup !== 'string' || !isRecord(stored.details)) return undefined;
  const { open, width } = stored.details;
  if (typeof open !== 'boolean' || typeof width !== 'number') return undefined;
  const layout: Layout = {
    root,
    activeGroup: stored.activeGroup,
    details: { open, width: clampWidth(width) },
    seq: highest + 1,
  };
  return normalize(layout);
}
