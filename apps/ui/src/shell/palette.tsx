// The command palette (Ctrl K): fuzzy search over projects, workstreams, tasks (by key and title)
// and sessions from the live query cache, plus the features' commands and "+ New" items. Keyboard
// first (the ARIA combobox pattern); results are virtualised. Loaded on demand.

import { useRouter } from '@tanstack/react-router';
import { useVirtualizer } from '@tanstack/react-virtual';
import { useId, useRef, useState, type KeyboardEvent } from 'react';
import { useProjects, useSessions, useTasks, useWorkstreams } from '../data/index.ts';
import {
  CommandIcon,
  ConsoleIcon,
  Dialog,
  DialogContent,
  FolderIcon,
  Kbd,
  PlusIcon,
  SearchIcon,
  WorkstreamIcon,
  CheckCircleIcon,
} from '../design/index.ts';
import { cx } from '../lib/cx.ts';
import { useRegistry } from './context.ts';
import { inLayout, type CommandContext } from './feature.ts';
import { rank } from './fuzzy.ts';
import { switchLayout, useLayout, useWorkspaceId } from './layout.ts';
import { paths } from './paths.ts';
import { useShell } from './store.ts';

type Kind = 'command' | 'create' | 'project' | 'workstream' | 'task' | 'session';

interface Item {
  id: string;
  kind: Kind;
  label: string;
  /** Shown before the label in mono: a task key, a project key. */
  code?: string;
  /** Shown at the end: the item's kind or its command group. */
  hint: string;
  keys?: readonly string[];
  fields: readonly string[];
  run(): void;
}

const ICONS = {
  command: CommandIcon,
  create: PlusIcon,
  project: FolderIcon,
  workstream: WorkstreamIcon,
  task: CheckCircleIcon,
  session: ConsoleIcon,
} as const;

const ROW_HEIGHT = 36;
const PAGE = 8;

/** Everything the palette finds. A "+ New" dialog it opens gives focus back to `opener`. */
function useItems(opener: Element | null): Item[] {
  const router = useRouter();
  const ws = useWorkspaceId();
  const layout = useLayout();
  const registry = useRegistry();
  const setCreating = useShell((s) => s.setCreating);
  const projects = useProjects().data ?? [];
  const workstreams = useWorkstreams().data ?? [];
  const tasks = useTasks().data ?? [];
  const sessions = useSessions().data ?? [];

  const go = (href: string) => void router.navigate({ href });
  const context: CommandContext = {
    workspace: ws,
    go: (path) => go(paths.under(ws, path)),
    switchLayout: (target) => switchLayout(router, ws, target),
    create: (id) => setCreating(id, opener),
  };
  const projectName = new Map(projects.map((p) => [p.id, p.name]));

  return [
    ...registry.commands
      .filter((c) => inLayout(c.layout, layout))
      .map<Item>((c) => ({
        id: `command:${c.id}`,
        kind: 'command',
        label: c.label,
        hint: c.group ?? 'Command',
        ...(c.keys === undefined ? {} : { keys: c.keys }),
        fields: [c.label, ...(c.keywords ?? [])],
        run: () => c.run(context),
      })),
    ...registry.create.map<Item>((e) => ({
      id: `create:${e.id}`,
      kind: 'create',
      label: `New ${e.label.toLowerCase()}`,
      hint: 'Create',
      fields: [`New ${e.label}`, 'create', 'add'],
      run: () => setCreating(e.id, opener),
    })),
    ...projects.map<Item>((p) => ({
      id: `project:${p.id}`,
      kind: 'project',
      label: p.name,
      code: p.key,
      hint: 'Project',
      fields: [p.name, p.key],
      run: () => go(paths.project(ws, p.id)),
    })),
    ...workstreams.map<Item>((w) => ({
      id: `workstream:${w.id}`,
      kind: 'workstream',
      label: w.name,
      hint: projectName.get(w.project) ?? 'Workstream',
      fields: [w.name, projectName.get(w.project) ?? ''],
      run: () => go(paths.workstream(ws, w.project, w.id)),
    })),
    ...tasks.map<Item>((t) => ({
      id: `task:${t.id}`,
      kind: 'task',
      label: t.title,
      code: t.key,
      hint: 'Task',
      fields: [t.key, t.title],
      run: () => go(paths.task(ws, t.key)),
    })),
    ...sessions.map<Item>((s) => ({
      id: `session:${s.id}`,
      kind: 'session',
      label: s.title ?? s.status_line ?? s.cwd,
      code: s.engine,
      hint: `Session · ${s.state}`,
      fields: [s.title ?? '', s.status_line ?? '', s.engine, s.branch ?? '', s.cwd],
      run: () => go(paths.session(ws, s.id)),
    })),
  ];
}

function Results({ close, opener }: { close(): void; opener: Element | null }) {
  // TanStack Virtual keeps state in a mutable object the compiler cannot see change.
  'use no memo';
  const items = useItems(opener);
  const [query, setQuery] = useState('');
  const [active, setActive] = useState(0);
  const listRef = useRef<HTMLDivElement>(null);
  const listId = useId();

  const results = query.trim() === '' ? items : rank(query, items, (item) => item.fields);
  const current = Math.min(active, Math.max(results.length - 1, 0));
  // eslint-disable-next-line react-hooks/incompatible-library -- opted out of memoisation above
  const virtualizer = useVirtualizer({
    count: results.length,
    getScrollElement: () => listRef.current,
    estimateSize: () => ROW_HEIGHT,
    overscan: 6,
  });
  const optionId = (index: number) => `${listId}-${index}`;

  function choose(item: Item | undefined) {
    if (item === undefined) return;
    close();
    item.run();
  }

  function moveTo(index: number) {
    if (results.length === 0) return;
    const next = Math.min(Math.max(index, 0), results.length - 1);
    setActive(next);
    virtualizer.scrollToIndex(next);
  }

  function onKeyDown(event: KeyboardEvent<HTMLInputElement>) {
    const moves: Record<string, number> = {
      ArrowDown: current + 1,
      ArrowUp: current - 1,
      PageDown: current + PAGE,
      PageUp: current - PAGE,
    };
    const to = moves[event.key];
    if (to !== undefined) {
      event.preventDefault();
      moveTo(to);
    } else if (event.key === 'Enter') {
      event.preventDefault();
      choose(results[current]);
    }
  }

  return (
    <>
      <div className="flex items-center gap-2 border-b border-line px-3">
        <SearchIcon className="text-ink-2" />
        <input
          role="combobox"
          aria-label="Search"
          aria-expanded={results.length > 0}
          aria-controls={listId}
          aria-autocomplete="list"
          aria-activedescendant={results.length > 0 ? optionId(current) : undefined}
          spellCheck={false}
          autoComplete="off"
          value={query}
          placeholder="Search projects, tasks and sessions, or run a command"
          onChange={(event) => {
            setQuery(event.target.value);
            setActive(0);
            virtualizer.scrollToOffset(0);
          }}
          onKeyDown={onKeyDown}
          className="h-12 min-w-0 flex-1 bg-transparent text-md text-ink outline-none placeholder:text-ink-2"
        />
        <Kbd keys={['esc']} />
      </div>
      {results.length === 0 ? (
        <p className="px-4 py-8 text-center text-sm text-ink-2">No matches.</p>
      ) : (
        <div
          ref={listRef}
          id={listId}
          role="listbox"
          aria-label="Results"
          className="max-h-[min(432px,56vh)] overflow-y-auto p-1"
        >
          <div role="presentation" className="relative w-full" style={{ height: virtualizer.getTotalSize() }}>
            {virtualizer.getVirtualItems().map((row) => {
              const item = results[row.index];
              if (item === undefined) return null;
              const Icon = ICONS[item.kind];
              const selected = row.index === current;
              return (
                <div
                  key={item.id}
                  id={optionId(row.index)}
                  role="option"
                  aria-selected={selected}
                  data-kind={item.kind}
                  onMouseMove={() => {
                    if (!selected) setActive(row.index);
                  }}
                  onClick={() => choose(item)}
                  style={{ height: row.size, transform: `translateY(${row.start}px)` }}
                  className={cx(
                    'absolute top-0 left-0 flex w-full cursor-default items-center gap-2 rounded-sm px-2 text-sm',
                    selected ? 'bg-hover text-ink' : 'text-ink',
                  )}
                >
                  <Icon className="text-ink-2" />
                  {item.code !== undefined && (
                    <span className="shrink-0 font-mono text-xs text-ink-2">{item.code}</span>
                  )}
                  <span className="min-w-0 flex-1 truncate">{item.label}</span>
                  {item.keys !== undefined && <Kbd keys={item.keys} />}
                  <span className="shrink-0 text-xs text-ink-2">{item.hint}</span>
                </div>
              );
            })}
          </div>
        </div>
      )}
      <p className="flex items-center gap-3 border-t border-line px-3 py-1.5 text-xs text-ink-2">
        <span className="inline-flex items-center gap-1">
          <Kbd keys={['↑']} />
          <Kbd keys={['↓']} /> to move
        </span>
        <span className="inline-flex items-center gap-1">
          <Kbd keys={['enter']} /> to open
        </span>
      </p>
    </>
  );
}

export function Palette() {
  const setOpen = useShell((s) => s.setPaletteOpen);
  // The palette has no trigger element, so it hands focus back itself: to what had it before, or
  // to the page when a result navigated.
  const [opener] = useState(() => document.activeElement);
  const hrefAtRun = useRef<string | null>(null);
  return (
    <Dialog open onOpenChange={setOpen}>
      <DialogContent
        title="Search and commands"
        hideTitle
        className="top-[10vh] w-[min(640px,calc(100vw-32px))]"
        onCloseAutoFocus={(event) => {
          event.preventDefault();
          // A "New …" result opened its dialog, which holds focus now.
          if (useShell.getState().creating !== null) return;
          const navigated = hrefAtRun.current !== null && hrefAtRun.current !== window.location.href;
          const target = navigated ? document.getElementById('main') : opener;
          if (target instanceof HTMLElement && target.isConnected) target.focus();
        }}
      >
        <Results
          opener={opener}
          close={() => {
            hrefAtRun.current = window.location.href;
            setOpen(false);
          }}
        />
      </DialogContent>
    </Dialog>
  );
}
