// The workbench: the console's session area as an IDE's editor area. Panes of tabs (sessions,
// terminals, files) side by side or stacked, tabs dragged between panes or to a pane's edge to
// split it, and the details sidebar. The layout is `layout.ts`'s, kept per workspace (`store.ts`).
//
// - Tabs follow the ARIA tabs pattern: one tab stop per pane; the arrows, Home and End move
//   between tabs (and show them); Shift with an arrow moves the tab; Delete closes it; Enter goes
//   into it; the Menu key (or a right-click) opens its actions. A middle click closes a tab, a
//   double-click keeps a preview tab.
// - The boundary between panes is a focusable separator: the arrows resize, Home and End go to
//   the limits.
// - Only the tab on screen in each pane is rendered. A file's unsaved edit waits in `drafts.ts`.

import { ContextMenu } from 'radix-ui';
import {
  Fragment,
  useEffect,
  useId,
  useRef,
  useState,
  type DragEvent,
  type KeyboardEvent,
  type PointerEvent,
  type ReactNode,
  type RefObject,
} from 'react';
import { FileOpenContext } from '../file-links.tsx';
import { FileExplorer } from '../../projects/file-explorer.tsx';
import { useWorkstreamById } from '../data.ts';
import { useSession } from '../../data/index.ts';
import { Button, CloseIcon, FOCUS_RING, PanelRightIcon, ResizablePanel, SplitDownIcon, SplitRightIcon } from '../../design/index.ts';
import { ariaShortcut } from '../../lib/platform.ts';
import { cx } from '../../lib/cx.ts';
import { sessionTitle } from '../format.ts';
import { Details } from './details.tsx';
import { dirtyTabs, forgetDraft, isDirty, useDrafts } from './drafts.ts';
import { FileTab } from './file-tab.tsx';
import { WORKBENCH_ACTIONS, workbenchActionFor, type WorkbenchAction } from './keys.ts';
import {
  activate,
  activeTabOf,
  closeGroup,
  closeOthers,
  closeTab,
  currentTab,
  DETAILS_WIDTH,
  dropTab,
  focusGroup,
  groupsOf,
  keep,
  LIMITS,
  moveTab,
  moveTabToGroup,
  openTab,
  resizeSplit,
  setDetails,
  setView,
  shiftTab,
  splitGroup,
  stepTab,
  type DropSide,
  type Group,
  type Layout,
  type LayoutNode,
  type SessionView,
  type Split,
  type Tab,
  type TabRef,
} from './layout.ts';
import { Placeholder, SessionPane } from './session-tab.tsx';
import { useWorkbenchLayout, workbenchStore, type WorkbenchStore } from './store.ts';

/** What a pane's content focuses first (F6 lands on the same elements). */
const CONTENT_FOCUS = '[data-chat-scroller], [data-terminal-focus], [data-work-focus], [data-file-focus]';

/** The tab being dragged, while it is; drags from anywhere else are ignored. */
let dragging: { group: string; tab: string } | undefined;
const DRAG_TYPE = 'application/x-pitcrew-tab';

export interface WorkbenchApi {
  ws: string;
  store: WorkbenchStore;
  layout: Layout;
  /**
   * Applies a change made in the workbench. The page follows the tab now on screen in its URL
   * (`follow`, the default), and focus goes to the active pane's tab or content.
   */
  change(update: (layout: Layout) => Layout, options?: { follow?: boolean; focus?: 'tab' | 'content' }): Layout;
  run(action: WorkbenchAction): void;
  /** Closes a tab, asking first when it holds unsaved changes. */
  close(group: string, tab: string): void;
  closeOthers(group: string, tab: string): void;
  closePane(group: string): void;
  openFile(ref: TabRef): void;
  openBeside(session: string, view: SessionView): void;
}

/** Whether the page may discard these tabs' unsaved edits; asks when there are any. */
function mayDiscard(ws: string, tabs: readonly string[]): boolean {
  const dirty = tabs.filter((t) => isDirty(ws, t));
  if (dirty.length === 0) return true;
  const ok = window.confirm(
    dirty.length === 1 ? 'Discard unsaved changes to this file?' : `Discard unsaved changes to ${dirty.length} files?`,
  );
  if (ok) for (const t of dirty) forgetDraft(ws, t);
  return ok;
}

export function useWorkbench(ws: string, onShow: (ref: TabRef | undefined) => void): WorkbenchApi {
  const store = workbenchStore(ws);
  const layout = useWorkbenchLayout(store);
  const timer = useRef<number | undefined>(undefined);
  useEffect(() => () => window.clearTimeout(timer.current), []);

  /** Focuses the active pane's tab (or its content) once it is drawn. */
  const focusLater = (where: 'tab' | 'content') => {
    window.clearTimeout(timer.current);
    let tries = 0;
    const attempt = () => {
      tries += 1;
      const group = store.get().activeGroup;
      // One workbench is on screen at a time.
      const pane = document.querySelector<HTMLElement>(`[data-workbench] [data-group="${group}"]`);
      const tab = pane?.querySelector<HTMLElement>('[role="tab"][aria-selected="true"]');
      const content = pane?.querySelector<HTMLElement>(CONTENT_FOCUS);
      const target = where === 'content' ? (content ?? (tries > 10 ? tab : undefined)) : tab;
      if (target != null) target.focus();
      else if (tries < 20) timer.current = window.setTimeout(attempt, 30);
    };
    timer.current = window.setTimeout(attempt, 0);
  };

  const change: WorkbenchApi['change'] = (update, options = {}) => {
    const before = store.get();
    const next = store.update(update);
    if (next !== before && options.follow !== false) onShow(currentTab(next)?.tab.ref);
    if (options.focus !== undefined) focusLater(options.focus);
    return next;
  };

  const close = (group: string, tab: string) => {
    if (!mayDiscard(ws, [tab])) return;
    forgetDraft(ws, tab);
    change((l) => closeTab(l, group, tab), { focus: 'tab' });
  };

  const api: WorkbenchApi = {
    ws,
    store,
    layout,
    change,
    close,
    closeOthers(group, tab) {
      const others = store.get();
      const tabs = groupsOf(others).find((g) => g.id === group)?.tabs.filter((t) => t.id !== tab) ?? [];
      if (!mayDiscard(ws, tabs.map((t) => t.id))) return;
      for (const t of tabs) forgetDraft(ws, t.id);
      change((l) => closeOthers(l, group, tab), { focus: 'tab' });
    },
    closePane(group) {
      const tabs = groupsOf(store.get()).find((g) => g.id === group)?.tabs ?? [];
      if (!mayDiscard(ws, tabs.map((t) => t.id))) return;
      for (const t of tabs) forgetDraft(ws, t.id);
      change((l) => closeGroup(l, group), { focus: 'tab' });
    },
    openFile(ref) {
      change((l) => openTab(l, ref), { focus: 'tab' });
    },
    openBeside(session, view) {
      change((l) => openTab(l, { kind: 'session', session, view }, { side: 'right' }), { focus: 'tab' });
    },
    run(action) {
      const current = currentTab(store.get());
      switch (action) {
        case 'next-tab':
        case 'previous-tab':
          change((l) => stepTab(l, action === 'next-tab' ? 1 : -1), { focus: 'tab' });
          return;
        case 'close-tab':
          if (current !== undefined) close(current.group.id, current.tab.id);
          return;
        case 'split-right':
        case 'split-down':
          change((l) => splitGroup(l, l.activeGroup, action === 'split-right' ? 'right' : 'down')[0], { focus: 'tab' });
          return;
        case 'move-to-next-pane':
        case 'move-to-previous-pane':
          change((l) => moveTabToGroup(l, action === 'move-to-next-pane' ? 1 : -1), { focus: 'tab' });
          return;
        case 'toggle-details':
          change((l) => setDetails(l, { open: !l.details.open }), { follow: false });
          return;
        case 'next-pane':
        case 'previous-pane':
          // The console moves between all its panes (F6); see console-page.tsx.
          return;
      }
    },
  };
  return api;
}

export interface WorkbenchProps {
  api: WorkbenchApi;
  /** Shown while nothing at all is open. */
  empty: ReactNode;
}

export function Workbench({ api, empty }: WorkbenchProps) {
  const { layout, ws } = api;
  const groups = groupsOf(layout);
  const numbers = new Map(groups.map((g, i) => [g.id, i + 1]));
  const current = currentTab(layout);
  const session = useSession(current?.tab.ref.kind === 'session' ? current.tab.ref.session : undefined).data;
  const streamId = current?.tab.ref.kind === 'file' ? current.tab.ref.workstream : session?.workstream;
  const stream = useWorkstreamById(streamId).data;
  useDrafts();
  const unsaved = dirtyTabs(ws).length > 0;
  useEffect(() => {
    if (!unsaved) return;
    const warn = (event: BeforeUnloadEvent) => {
      event.preventDefault();
      event.returnValue = '';
    };
    window.addEventListener('beforeunload', warn);
    return () => window.removeEventListener('beforeunload', warn);
  }, [unsaved]);

  const onKeyDown = (event: KeyboardEvent<HTMLDivElement>) => {
    const action = workbenchActionFor(event);
    if (action === undefined) return;
    event.preventDefault();
    if (!event.repeat) api.run(action);
  };

  return (
    <FileOpenContext.Provider value={api.openFile}>
    <div data-workbench="" onKeyDown={onKeyDown} className="flex min-h-0 min-w-0 flex-1">
      {stream && <div className="w-60 shrink-0 overflow-auto"><FileExplorer key={stream.id} workstream={stream} openFile={api.openFile} /></div>}
      {/* The panes are the landmarks ("Pane 1"…); this is only their frame. */}
      <div className="flex min-h-0 min-w-0 flex-1 flex-col">
        <NodeView api={api} node={layout.root} numbers={numbers} count={groups.length} empty={empty} />
      </div>
      {layout.details.open && (
        <ResizablePanel
          as="aside"
          side="right"
          label="Details"
          width={layout.details.width}
          onWidthChange={(width) => api.change((l) => setDetails(l, { width }), { follow: false })}
          min={DETAILS_WIDTH.min}
          max={DETAILS_WIDTH.max}
          className="border-l border-line bg-sidebar"
        >
          <Details ws={ws} tab={current?.tab} onOpenFile={api.openFile} onOpenBeside={api.openBeside} />
        </ResizablePanel>
      )}
    </div>
    </FileOpenContext.Provider>
  );
}

interface NodeProps {
  api: WorkbenchApi;
  numbers: Map<string, number>;
  count: number;
  empty: ReactNode;
}

function NodeView({ node, ...props }: NodeProps & { node: LayoutNode }) {
  return node.type === 'group' ? <GroupView group={node} {...props} /> : <SplitView split={node} {...props} />;
}

function SplitView({ split, ...props }: NodeProps & { split: Split }) {
  const box = useRef<HTMLDivElement>(null);
  return (
    <div
      ref={box}
      data-split={split.id}
      className={cx('flex min-h-0 min-w-0 flex-1', split.direction === 'row' ? 'flex-row' : 'flex-col')}
    >
      {split.children.map((child, i) => (
        <Fragment key={child.id}>
          {i > 0 && <Splitter api={props.api} split={split} index={i - 1} box={box} />}
          <div className="flex min-h-0 min-w-0" style={{ flex: `${split.sizes[i] ?? 1} 1 0px` }}>
            <NodeView node={child} {...props} />
          </div>
        </Fragment>
      ))}
    </div>
  );
}

const STEP = 0.05;

function Splitter({ api, split, index, box }: { api: WorkbenchApi; split: Split; index: number; box: RefObject<HTMLDivElement | null> }) {
  const last = useRef<number | null>(null);
  const row = split.direction === 'row';
  const position = Math.round(split.sizes.slice(0, index + 1).reduce((a, b) => a + b, 0) * 100);
  const resize = (delta: number) => api.change((l) => resizeSplit(l, split.id, index, delta), { follow: false });
  const onKeyDown = (event: KeyboardEvent<HTMLDivElement>) => {
    const deltas: Record<string, number> = row
      ? { ArrowLeft: -STEP, ArrowRight: STEP, Home: -1, End: 1 }
      : { ArrowUp: -STEP, ArrowDown: STEP, Home: -1, End: 1 };
    const delta = deltas[event.key];
    if (delta === undefined) return;
    event.preventDefault();
    resize(delta);
  };
  const along = (event: PointerEvent<HTMLDivElement>) => (row ? event.clientX : event.clientY);
  return (
    <div
      role="separator"
      tabIndex={0}
      aria-orientation={row ? 'vertical' : 'horizontal'}
      aria-label={row ? 'Resize the panes side by side' : 'Resize the stacked panes'}
      aria-valuenow={position}
      aria-valuemin={0}
      aria-valuemax={100}
      data-splitter={split.id}
      onKeyDown={onKeyDown}
      onPointerDown={(event) => {
        if (event.button !== 0) return;
        event.preventDefault();
        event.currentTarget.setPointerCapture(event.pointerId);
        last.current = along(event);
      }}
      onPointerMove={(event) => {
        const from = last.current;
        const rect = box.current?.getBoundingClientRect();
        if (from === null || rect === undefined) return;
        const size = row ? rect.width : rect.height;
        if (size <= 0) return;
        const at = along(event);
        last.current = at;
        resize((at - from) / size);
      }}
      onPointerUp={(event) => {
        last.current = null;
        if (event.currentTarget.hasPointerCapture(event.pointerId)) event.currentTarget.releasePointerCapture(event.pointerId);
      }}
      onPointerCancel={() => {
        last.current = null;
      }}
      className={cx(
        'group relative z-10 shrink-0 touch-none bg-line outline-none',
        row ? '-mx-[3px] w-[7px] cursor-col-resize border-x-[3px] border-transparent bg-clip-padding' : '-my-[3px] h-[7px] cursor-row-resize border-y-[3px] border-transparent bg-clip-padding',
        'hover:bg-line-2 focus-visible:bg-accent',
      )}
    />
  );
}

function zoneOf(event: DragEvent<HTMLElement>): DropSide {
  const box = event.currentTarget.getBoundingClientRect();
  if (box.width <= 0 || box.height <= 0) return 'center';
  const x = (event.clientX - box.left) / box.width;
  const y = (event.clientY - box.top) / box.height;
  const edges: [DropSide, number][] = [
    ['left', x],
    ['right', 1 - x],
    ['top', y],
    ['bottom', 1 - y],
  ];
  edges.sort((a, b) => a[1] - b[1]);
  const [side, distance] = edges[0] as [DropSide, number];
  return distance < 0.25 ? side : 'center';
}

const ZONE_CLASS: Record<DropSide, string> = {
  center: 'inset-1',
  left: 'inset-y-1 left-1 w-1/2',
  right: 'inset-y-1 right-1 w-1/2',
  top: 'inset-x-1 top-1 h-1/2',
  bottom: 'inset-x-1 bottom-1 h-1/2',
};

function GroupView({ group, api, numbers, count, empty }: NodeProps & { group: Group }) {
  const number = numbers.get(group.id) ?? 1;
  const active = api.layout.activeGroup === group.id;
  const tab = activeTabOf(group);
  const prefix = useId();
  const idOf = (t: Tab) => `${prefix}tab-${t.id}`;
  const panelId = `${prefix}panel`;
  const [zone, setZone] = useState<DropSide | null>(null);
  const takeFocus = () => {
    if (!active) api.change((l) => focusGroup(l, group.id));
  };
  const onlyPane = count === 1;
  const dropHandlers = {
    onDragOver: (event: DragEvent<HTMLDivElement>) => {
      if (dragging === undefined) return;
      event.preventDefault();
      event.dataTransfer.dropEffect = 'move';
      setZone(zoneOf(event));
    },
    onDragLeave: (event: DragEvent<HTMLDivElement>) => {
      if (!event.currentTarget.contains(event.relatedTarget as Node | null)) setZone(null);
    },
    onDrop: (event: DragEvent<HTMLDivElement>) => {
      const from = dragging;
      setZone(null);
      if (from === undefined) return;
      event.preventDefault();
      const side = zoneOf(event);
      api.change((l) => dropTab(l, from, group.id, side), { focus: 'tab' });
    },
  };
  const toolButton = 'size-7 justify-center px-0';
  return (
    <section
      aria-label={`Pane ${number}`}
      data-group={group.id}
      data-active-group={active ? '' : undefined}
      onFocus={takeFocus}
      onPointerDown={takeFocus}
      className="flex min-h-0 min-w-0 flex-1 flex-col"
    >
      <div className="flex h-9 shrink-0 items-stretch border-b border-line bg-sidebar">
        <TabStrip api={api} group={group} number={number} active={active} idOf={idOf} panelId={panelId} />
        <div className="flex shrink-0 items-center gap-0.5 px-1">
          <Button
            variant="ghost"
            className={toolButton}
            aria-label="Split right"
            title={`${WORKBENCH_ACTIONS['split-right'].label} (Alt \\)`}
            aria-keyshortcuts={active ? ariaShortcut(WORKBENCH_ACTIONS['split-right'].keys ?? []) : undefined}
            disabled={count >= LIMITS.groups}
            onClick={() => api.change((l) => splitGroup(l, group.id, 'right')[0], { focus: 'tab' })}
          >
            <SplitRightIcon />
          </Button>
          <Button
            variant="ghost"
            className={toolButton}
            aria-label="Split down"
            title={`${WORKBENCH_ACTIONS['split-down'].label} (Alt Shift \\)`}
            aria-keyshortcuts={active ? ariaShortcut(WORKBENCH_ACTIONS['split-down'].keys ?? []) : undefined}
            disabled={count >= LIMITS.groups}
            onClick={() => api.change((l) => splitGroup(l, group.id, 'down')[0], { focus: 'tab' })}
          >
            <SplitDownIcon />
          </Button>
          <Button
            variant="ghost"
            className={toolButton}
            aria-label="Details"
            title="Show or hide the details"
            aria-pressed={api.layout.details.open}
            onClick={() => api.change((l) => setDetails(focusGroup(l, group.id), { open: !l.details.open }))}
          >
            <PanelRightIcon />
          </Button>
        </div>
      </div>
      <div
        {...(tab === undefined ? {} : { role: 'tabpanel', id: panelId, 'aria-labelledby': idOf(tab) })}
        data-drop-zone={zone ?? undefined}
        className="relative flex min-h-0 flex-1 flex-col"
        {...dropHandlers}
      >
        {tab === undefined ? (
          onlyPane ? (
            empty
          ) : (
            <Placeholder title="Nothing open here">
              <p>Choose a session from the list, or drag a tab here.</p>
              <Button className="mt-3" onClick={() => api.closePane(group.id)}>
                Close the pane
              </Button>
            </Placeholder>
          )
        ) : tab.ref.kind === 'session' ? (
          <SessionPane
            key={tab.id}
            ws={api.ws}
            sessionId={tab.ref.session}
            view={tab.ref.view}
            onView={(view) => api.change((l) => setView(l, group.id, tab.id, view))}
            narrow={false}
            pane={number}
          />
        ) : (
          <FileTab
            paneNumber={number}
            key={tab.id}
            ws={api.ws}
            tabId={tab.id}
            workstream={tab.ref.workstream}
            location={tab.ref.location}
            path={tab.ref.path}
            line={tab.ref.line}
          />
        )}
        {zone !== null && (
          <div aria-hidden className={cx('pointer-events-none absolute z-20 rounded-md border-2 border-accent bg-accent-soft opacity-70', ZONE_CLASS[zone])} />
        )}
      </div>
    </section>
  );
}

const VIEW_LABEL: Record<SessionView, string> = { chat: 'Chat', terminal: 'Terminal', work: 'Work' };

function SessionLabel({ session, view, preview }: { session: string; view: SessionView; preview: boolean }) {
  const found = useSession(session);
  const title = found.data !== undefined ? sessionTitle(found.data) : found.error !== null ? 'Unknown session' : 'Session…';
  return (
    <>
      <span className={cx('min-w-0 truncate', preview && 'italic')}>{title}</span>
      {view !== 'chat' && <span className="shrink-0 text-ink-2">· {VIEW_LABEL[view]}</span>}
    </>
  );
}

function FileLabel({ path, preview }: { path: string; preview: boolean }) {
  return <span className={cx('min-w-0 truncate', preview && 'italic')}>{path.split('/').pop() || path}</span>;
}

const MENU_ITEM =
  'flex cursor-default items-center justify-between gap-6 rounded-sm px-2 py-1.5 text-sm outline-none select-none data-disabled:text-ink-2 data-disabled:opacity-60 data-highlighted:bg-hover';

function TabStrip({
  api,
  group,
  number,
  active,
  idOf,
  panelId,
}: {
  api: WorkbenchApi;
  group: Group;
  number: number;
  active: boolean;
  idOf(tab: Tab): string;
  panelId: string;
}) {
  const strip = useRef<HTMLDivElement>(null);
  const hint = useId();
  const [insertAt, setInsertAt] = useState<number | null>(null);
  const { ws } = api;
  const insertIndex = (event: DragEvent<HTMLDivElement>) => {
    const tabs = [...(strip.current?.querySelectorAll<HTMLElement>('[role="tab"]') ?? [])];
    const at = tabs.findIndex((t) => {
      const box = t.getBoundingClientRect();
      return event.clientX < box.left + box.width / 2;
    });
    return at === -1 ? tabs.length : at;
  };
  const onKeyDown = (event: KeyboardEvent<HTMLDivElement>, tab: Tab, index: number) => {
    const n = group.tabs.length;
    const go = (target: Tab | undefined) => {
      if (target !== undefined) api.change((l) => activate(l, group.id, target.id), { focus: 'tab' });
    };
    if (event.altKey || event.ctrlKey || event.metaKey) return;
    switch (event.key) {
      case 'ArrowRight':
      case 'ArrowLeft': {
        const step = event.key === 'ArrowRight' ? 1 : -1;
        if (event.shiftKey) api.change((l) => shiftTab(l, group.id, tab.id, step), { focus: 'tab' });
        else go(group.tabs[(index + step + n) % n]);
        break;
      }
      case 'Home':
        go(group.tabs[0]);
        break;
      case 'End':
        go(group.tabs[n - 1]);
        break;
      case 'Delete':
        api.close(group.id, tab.id);
        break;
      case 'Enter':
        api.change((l) => activate(l, group.id, tab.id), { focus: 'content' });
        break;
      default:
        return;
    }
    event.preventDefault();
  };
  return (
    <div
      ref={strip}
      role="tablist"
      aria-label={`Tabs in pane ${number}`}
      className="flex min-w-0 flex-1 overflow-x-auto"
      onDragOver={(event) => {
        if (dragging === undefined) return;
        event.preventDefault();
        event.dataTransfer.dropEffect = 'move';
        setInsertAt(insertIndex(event));
      }}
      onDragLeave={(event) => {
        if (!event.currentTarget.contains(event.relatedTarget as Node | null)) setInsertAt(null);
      }}
      onDrop={(event) => {
        const from = dragging;
        const index = insertIndex(event);
        setInsertAt(null);
        if (from === undefined) return;
        event.preventDefault();
        api.change((l) => moveTab(l, from, { group: group.id, index }), { focus: 'tab' });
      }}
    >
      <span id={hint} hidden>
        Arrow keys switch tabs; Shift with an arrow moves the tab; Delete closes it; Enter goes into it.
      </span>
      {group.tabs.map((tab, index) => {
        const selected = tab.id === group.active;
        const dirty = isDirty(ws, tab.id);
        return (
          <ContextMenu.Root key={tab.id} modal={false}>
            <ContextMenu.Trigger asChild>
              <div
                role="tab"
                id={idOf(tab)}
                aria-selected={selected}
                aria-controls={selected ? panelId : undefined}
                aria-describedby={hint}
                aria-keyshortcuts="Delete"
                tabIndex={selected ? 0 : -1}
                draggable
                data-tab={tab.id}
                data-preview={tab.preview ? '' : undefined}
                data-drop-before={insertAt === index ? '' : undefined}
                data-drop-after={insertAt === group.tabs.length && index === group.tabs.length - 1 ? '' : undefined}
                title={tab.ref.kind === 'file' ? tab.ref.path : undefined}
                onClick={() => api.change((l) => activate(l, group.id, tab.id))}
                onDoubleClick={() => api.change((l) => keep(l, group.id, tab.id))}
                onMouseDown={(event) => {
                  // A middle click closes, without the browser's autoscroll.
                  if (event.button === 1) event.preventDefault();
                }}
                onAuxClick={(event) => {
                  if (event.button !== 1) return;
                  event.preventDefault();
                  api.close(group.id, tab.id);
                }}
                onKeyDown={(event) => onKeyDown(event, tab, index)}
                onDragStart={(event) => {
                  dragging = { group: group.id, tab: tab.id };
                  event.dataTransfer.effectAllowed = 'move';
                  event.dataTransfer.setData(DRAG_TYPE, tab.id);
                }}
                onDragEnd={() => {
                  dragging = undefined;
                  setInsertAt(null);
                }}
                className={cx(
                  'relative flex max-w-60 min-w-0 shrink-0 cursor-default items-center gap-1.5 border-r border-line pr-1 pl-3 text-sm outline-none select-none',
                  FOCUS_RING,
                  'focus-visible:-outline-offset-2',
                  selected ? 'bg-bg text-ink' : 'text-ink-2 hover:bg-hover hover:text-ink',
                  // The tab on screen in the active pane is marked along its top edge.
                  selected && active && 'shadow-[inset_0_2px_0_var(--pc-accent)]',
                  'data-[drop-before]:shadow-[inset_2px_0_0_var(--pc-accent)] data-[drop-after]:shadow-[inset_-2px_0_0_var(--pc-accent)]',
                )}
              >
                {tab.ref.kind === 'session' ? (
                  <SessionLabel session={tab.ref.session} view={tab.ref.view} preview={tab.preview} />
                ) : (
                  <FileLabel path={tab.ref.path} preview={tab.preview} />
                )}
                {tab.preview && <span className="sr-only">(preview)</span>}
                {dirty && (
                  <>
                    <span aria-hidden className="text-accent-text">
                      ●
                    </span>
                    <span className="sr-only">(unsaved)</span>
                  </>
                )}
                <span
                  aria-hidden
                  data-close-tab=""
                  title="Close (Delete)"
                  className="ml-0.5 flex size-5 shrink-0 items-center justify-center rounded-sm text-ink-2 hover:bg-hover hover:text-ink"
                  onPointerDown={(event) => event.stopPropagation()}
                  onClick={(event) => {
                    event.stopPropagation();
                    api.close(group.id, tab.id);
                  }}
                >
                  <CloseIcon className="size-3" />
                </span>
              </div>
            </ContextMenu.Trigger>
            <ContextMenu.Portal>
              <ContextMenu.Content
                className="z-50 min-w-52 rounded-md border border-line bg-card p-1 shadow-pop"
                // Each action puts focus where its result is (the tab may have moved or gone).
                onCloseAutoFocus={(event) => event.preventDefault()}
              >
                {tab.preview && (
                  <ContextMenu.Item className={MENU_ITEM} onSelect={() => api.change((l) => keep(l, group.id, tab.id), { focus: 'tab' })}>
                    Keep open
                  </ContextMenu.Item>
                )}
                <ContextMenu.Item className={MENU_ITEM} onSelect={() => api.close(group.id, tab.id)}>
                  Close
                </ContextMenu.Item>
                <ContextMenu.Item
                  className={MENU_ITEM}
                  disabled={group.tabs.length < 2}
                  onSelect={() => api.closeOthers(group.id, tab.id)}
                >
                  Close the others
                </ContextMenu.Item>
                <ContextMenu.Separator className="my-1 h-px bg-line" />
                <ContextMenu.Item
                  className={MENU_ITEM}
                  onSelect={() => api.change((l) => splitGroup(activate(l, group.id, tab.id), group.id, 'right')[0], { focus: 'tab' })}
                >
                  Split right
                </ContextMenu.Item>
                <ContextMenu.Item
                  className={MENU_ITEM}
                  onSelect={() => api.change((l) => splitGroup(activate(l, group.id, tab.id), group.id, 'down')[0], { focus: 'tab' })}
                >
                  Split down
                </ContextMenu.Item>
                <ContextMenu.Item
                  className={MENU_ITEM}
                  onSelect={() => api.change((l) => moveTabToGroup(activate(l, group.id, tab.id), 1), { focus: 'tab' })}
                >
                  Move to the next pane
                </ContextMenu.Item>
              </ContextMenu.Content>
            </ContextMenu.Portal>
          </ContextMenu.Root>
        );
      })}
    </div>
  );
}
