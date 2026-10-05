// The session list: every session, grouped by project and workstream plus Unsorted, virtualised.
// Arrow keys, Home, End and Page keys move through sessions (and select them, if the caller wants
// the selection to follow); Enter or a click selects one.

import { useVirtualizer } from '@tanstack/react-virtual';
import { useEffect, useMemo, useRef, useState, type KeyboardEvent } from 'react';
import { ContextMenu } from 'radix-ui';
import { LinkSessionDialog } from './link-session.tsx';
import { ApiError, useMembers, type Member, type Session } from '../data/index.ts';
import { EngineLogo, StatusPill } from '../design/index.ts';
import { cx } from '../lib/cx.ts';
import { useConsoleSessions } from './data.ts';
import { NO_FACETS, placeOf, UNSORTED, type SessionFacets, type SessionPlaces } from './facets.ts';
import { ENGINE_LABEL, relativeTime, sessionTitle, STATE, useNow } from './format.ts';

export type ListRow =
  | { type: 'project'; key: string; label: string; count: number }
  | { type: 'workstream'; key: string; label: string; count: number }
  | { type: 'session'; key: string; session: Session };

const NO_WORKSTREAM = '';

/** Live sessions first, then by last activity, newest first. */
function bySessionOrder(a: Session, b: Session): number {
  const ended = Number(a.state === 'ended') - Number(b.state === 'ended');
  return ended !== 0 ? ended : b.last_activity - a.last_activity || a.id.localeCompare(b.id);
}

/**
 * Rows for the list: each project (in the workspace's order) with its workstreams, then Unsorted
 * for sessions in no project. Only groups with sessions appear.
 */
export function groupSessions(sessions: readonly Session[], places: SessionPlaces): ListRow[] {
  const groups = new Map<string, Map<string, Session[]>>();
  for (const session of sessions) {
    const { project, workstream } = placeOf(session, places);
    const projectKey = project?.id ?? UNSORTED;
    const workstreamKey = project === undefined ? NO_WORKSTREAM : (workstream?.id ?? NO_WORKSTREAM);
    let byWorkstream = groups.get(projectKey);
    if (byWorkstream === undefined) {
      byWorkstream = new Map();
      groups.set(projectKey, byWorkstream);
    }
    const list = byWorkstream.get(workstreamKey);
    if (list === undefined) byWorkstream.set(workstreamKey, [session]);
    else list.push(session);
  }

  const rows: ListRow[] = [];
  const count = (byWorkstream: Map<string, Session[]>) =>
    [...byWorkstream.values()].reduce((sum, list) => sum + list.length, 0);
  const pushSessions = (list: Session[]) => {
    for (const session of list.sort(bySessionOrder)) rows.push({ type: 'session', key: session.id, session });
  };

  for (const project of places.projects) {
    const byWorkstream = groups.get(project.id);
    if (byWorkstream === undefined) continue;
    rows.push({ type: 'project', key: `project:${project.id}`, label: project.name, count: count(byWorkstream) });
    const order = [
      ...places.workstreams.filter((w) => w.project === project.id).map((w) => [w.id, w.name] as const),
      [NO_WORKSTREAM, 'No workstream'] as const,
    ];
    for (const [id, name] of order) {
      const list = byWorkstream.get(id);
      if (list === undefined) continue;
      rows.push({ type: 'workstream', key: `workstream:${project.id}:${id}`, label: name, count: list.length });
      pushSessions(list);
    }
  }
  const unsorted = groups.get(UNSORTED);
  if (unsorted !== undefined) {
    rows.push({ type: 'project', key: `project:${UNSORTED}`, label: 'Unsorted', count: count(unsorted) });
    pushSessions([...unsorted.values()].flat());
  }
  return rows;
}

const ROW_HEIGHT = 56;
const PROJECT_HEIGHT = 32;
const WORKSTREAM_HEIGHT = 26;
const PAGE = 10;

/** How a session was chosen: a click, or Enter or Space in the list. */
export type SelectVia = 'pointer' | 'keyboard';

/** Where a row's menu opens its session: a new tab, or a new pane to the side. */
export type OpenWhere = 'tab' | 'side';

export interface SessionListViewProps {
  sessions: readonly Session[];
  places: SessionPlaces;
  members?: readonly Member[] | undefined;
  selectedId?: string | undefined;
  onSelect: (session: Session, via: SelectVia) => void;
  onLink?: ((session: Session) => void) | undefined;
  /** Adds "Open in a new tab" and "Open to the side" to a row's menu. */
  onOpen?: ((session: Session, where: OpenWhere) => void) | undefined;
  /**
   * The arrow, Page, Home and End keys moved to `session`. Pass it to make the selection follow
   * the keys (as the console does when the chosen session shows beside the list).
   */
  onActiveChange?: ((session: Session) => void) | undefined;
  /** Shown when there are no sessions. */
  empty?: string;
  className?: string;
  'aria-label'?: string;
}

/** The list itself, from data it is given. `SessionList` feeds it from the hub. */
export function SessionListView(props: SessionListViewProps) {
  'use no memo'; // TanStack Virtual's instance changes under the React Compiler's memoisation.
  const { sessions, places, selectedId, onSelect, onActiveChange } = props;
  const rows = useMemo(() => groupSessions(sessions, places), [sessions, places]);
  const handles = useMemo(
    () => new Map((props.members ?? []).map((m) => [m.id, m.handle] as const)),
    [props.members],
  );
  const now = useNow();
  const scrollRef = useRef<HTMLDivElement>(null);
  // eslint-disable-next-line react-hooks/incompatible-library -- this component opts out of the compiler ('use no memo').
  const virtualizer = useVirtualizer({
    count: rows.length,
    getScrollElement: () => scrollRef.current,
    estimateSize: (i) => {
      const type = rows[i]?.type;
      return type === 'session' ? ROW_HEIGHT : type === 'project' ? PROJECT_HEIGHT : WORKSTREAM_HEIGHT;
    },
    getItemKey: (i) => rows[i]?.key ?? i,
    overscan: 8,
  });

  const [active, setActive] = useState<string | undefined>(selectedId);
  // A selection made elsewhere (a link, the palette) moves the keyboard's place to it.
  const [seenSelected, setSeenSelected] = useState(selectedId);
  if (selectedId !== seenSelected) {
    setSeenSelected(selectedId);
    if (selectedId !== undefined) setActive(selectedId);
  }
  const sessionIndexes = useMemo(
    () => rows.flatMap((row, i) => (row.type === 'session' ? [i] : [])),
    [rows],
  );
  const activeRow = active === undefined ? -1 : rows.findIndex((r) => r.type === 'session' && r.session.id === active);
  const activeId = activeRow === -1 ? undefined : active;

  // Brings a newly selected session into view, once; later updates to the list leave the scroll be.
  const scrolledTo = useRef<string | undefined>(undefined);
  useEffect(() => {
    if (selectedId === undefined || scrolledTo.current === selectedId) return;
    const index = rows.findIndex((r) => r.type === 'session' && r.session.id === selectedId);
    if (index === -1) return;
    scrolledTo.current = selectedId;
    virtualizer.scrollToIndex(index, { align: 'auto' });
  }, [selectedId, rows, virtualizer]);

  /** Moves the keyboard's place to row `index`; `follow` also tells `onActiveChange`. */
  const moveTo = (index: number | undefined, follow = true) => {
    const row = index === undefined ? undefined : rows[index];
    if (index === undefined || row?.type !== 'session') return;
    setActive(row.session.id);
    virtualizer.scrollToIndex(index, { align: 'auto' });
    if (follow && row.session.id !== active) onActiveChange?.(row.session);
  };

  const onKeyDown = (event: KeyboardEvent<HTMLDivElement>) => {
    const at = sessionIndexes.indexOf(activeRow);
    const step = (delta: number) => {
      const next = at === -1 ? (delta > 0 ? 0 : sessionIndexes.length - 1) : at + delta;
      moveTo(sessionIndexes[Math.max(0, Math.min(sessionIndexes.length - 1, next))]);
    };
    switch (event.key) {
      case 'ArrowDown':
        step(1);
        break;
      case 'ArrowUp':
        step(-1);
        break;
      case 'PageDown':
        step(PAGE);
        break;
      case 'PageUp':
        step(-PAGE);
        break;
      case 'Home':
        moveTo(sessionIndexes[0]);
        break;
      case 'End':
        moveTo(sessionIndexes.at(-1));
        break;
      case 'Enter':
      case ' ': {
        const row = rows[activeRow];
        if (row?.type === 'session') onSelect(row.session, 'keyboard');
        break;
      }
      default:
        return;
    }
    event.preventDefault();
  };

  if (rows.length === 0) {
    return (
      <div className={cx('flex h-full items-center justify-center p-6 text-sm text-ink-2', props.className)}>
        {props.empty ?? 'No sessions.'}
      </div>
    );
  }

  return (
    <div
      ref={scrollRef}
      data-virtual-scroller=""
      className={cx('h-full overflow-y-auto', props.className)}
    >
      <div
        role="listbox"
        tabIndex={0}
        aria-label={props['aria-label'] ?? 'Sessions'}
        aria-activedescendant={activeId === undefined ? undefined : optionId(activeId)}
        onKeyDown={onKeyDown}
        onFocus={() => {
          // Focus alone marks a place to start from; it selects nothing.
          if (activeId === undefined) {
            moveTo(sessionIndexes.find((i) => rows[i]?.key === selectedId) ?? sessionIndexes[0], false);
          }
        }}
        className="group relative w-full outline-none"
        style={{ height: virtualizer.getTotalSize() }}
      >
        {virtualizer.getVirtualItems().map((item) => {
          const row = rows[item.index];
          if (row === undefined) return null;
          return (
            <div
              key={item.key}
              data-index={item.index}
              className="absolute top-0 left-0 w-full"
              style={{ height: item.size, transform: `translateY(${item.start}px)` }}
              role="presentation"
            >
              {row.type === 'session' ? (
                <SessionRow
                  session={row.session}
                  handle={row.session.agent === undefined ? undefined : handles.get(row.session.agent)}
                  onLink={props.onLink === undefined ? undefined : () => props.onLink?.(row.session)}
                  onOpen={props.onOpen === undefined ? undefined : (where) => props.onOpen?.(row.session, where)}
                  now={now}
                  selected={row.session.id === selectedId}
                  active={row.session.id === activeId}
                  onClick={() => {
                    setActive(row.session.id);
                    onSelect(row.session, 'pointer');
                  }}
                />
              ) : (
                <GroupHeader row={row} />
              )}
            </div>
          );
        })}
      </div>
    </div>
  );
}

const optionId = (sessionId: string) => `pc-session-option-${sessionId}`;

function GroupHeader({ row }: { row: Extract<ListRow, { type: 'project' | 'workstream' }> }) {
  return row.type === 'project' ? (
    <div className="flex h-full items-end gap-2 px-3 pb-1 text-xs font-semibold tracking-wide text-ink-2 uppercase">
      <span className="truncate">{row.label}</span>
      <span className="font-normal text-ink-2">{row.count}</span>
    </div>
  ) : (
    <div className="flex h-full items-center gap-2 pr-3 pl-5 text-xs text-ink-2">
      <span className="truncate">{row.label}</span>
      <span>{row.count}</span>
    </div>
  );
}

function SessionRow(props: {
  session: Session;
  handle: string | undefined;
  now: number;
  selected: boolean;
  active: boolean;
  onClick: () => void;
  onLink: (() => void) | undefined;
  onOpen: ((where: OpenWhere) => void) | undefined;
}) {
  const { session } = props;
  // Opening a tab moves focus to it: the menu must not hand focus back to the row afterwards.
  const opened = useRef(false);
  const starting = session.state === 'starting';
  const state = STATE[session.state];
  const row = (
    <div
      id={optionId(session.id)}
      role="option"
      aria-selected={props.selected}
      data-session={session.id}
      data-state={session.state}
      onClick={props.onClick}
      className={cx(
        'mx-1 flex h-[52px] cursor-pointer flex-col justify-center gap-0.5 rounded-md px-3',
        props.selected ? 'bg-accent-soft' : 'hover:bg-hover',
        // The keyboard's place in the list, shown while the list has keyboard focus.
        props.active && 'group-focus-visible:ring-2 group-focus-visible:ring-accent group-focus-visible:ring-inset',
      )}
    >
      <div className="flex min-w-0 items-center gap-2">
        <EngineLogo engine={session.engine} className="text-ink-2" />
        <span className="shrink-0 font-mono text-[10px] tracking-wide text-ink-2 uppercase">
          {ENGINE_LABEL[session.engine]}
        </span>
        <span className={cx('min-w-0 flex-1 truncate text-sm font-medium', session.state === 'ended' && 'text-ink-2')}>
          {sessionTitle(session)}
        </span>
        <time dateTime={new Date(session.last_activity).toISOString()} className="shrink-0 text-xs text-ink-2">
          {relativeTime(session.last_activity, props.now)}
        </time>
      </div>
      <div className="flex min-w-0 items-center gap-2 text-xs">
        {props.handle !== undefined && <span className="shrink-0 text-ink-2">{props.handle}</span>}
        {starting ? (
          <span className="flex items-center gap-1.5 text-accent-text" data-testid="starting">
            <span aria-hidden className="size-1.5 animate-pulse rounded-pill bg-accent" />
            Starting…
          </span>
        ) : (
          <>
            <StatusPill tone={state.tone}>{state.label}</StatusPill>
            {session.status_line !== undefined && (
              <span className="min-w-0 truncate text-ink-2">{session.status_line}</span>
            )}
          </>
        )}
      </div>
    </div>
  );
  if (props.onLink === undefined && props.onOpen === undefined) return row;
  const item = 'rounded-sm px-2 py-1.5 text-sm outline-none data-highlighted:bg-hover';
  const { onOpen } = props;
  const open = (where: OpenWhere) => {
    opened.current = true;
    onOpen?.(where);
  };
  return (
    <ContextMenu.Root modal={false}>
      <ContextMenu.Trigger asChild>{row}</ContextMenu.Trigger>
      <ContextMenu.Portal>
        <ContextMenu.Content
          className="z-50 min-w-44 rounded-md border border-line bg-card p-1 shadow-pop"
          onCloseAutoFocus={(event) => {
            if (opened.current) event.preventDefault();
            opened.current = false;
          }}
        >
          {onOpen !== undefined && (
            <>
              <ContextMenu.Item onSelect={() => open('tab')} className={item}>Open in a new tab</ContextMenu.Item>
              <ContextMenu.Item onSelect={() => open('side')} className={item}>Open to the side</ContextMenu.Item>
            </>
          )}
          {props.onLink !== undefined && <ContextMenu.Item onSelect={props.onLink} className={item}>Link to…</ContextMenu.Item>}
        </ContextMenu.Content>
      </ContextMenu.Portal>
    </ContextMenu.Root>
  );
}

export interface SessionListProps {
  facets?: SessionFacets;
  selectedId?: string | undefined;
  onSelect: (session: Session, via: SelectVia) => void;
  /** See `SessionListViewProps.onOpen`. */
  onOpen?: ((session: Session, where: OpenWhere) => void) | undefined;
  /** See `SessionListViewProps.onActiveChange`. */
  onActiveChange?: ((session: Session) => void) | undefined;
  className?: string;
}

/** The session list, live from the hub, filtered by `facets`. */
export function SessionList(props: SessionListProps) {
  const [linking, setLinking] = useState<Session | undefined>();
  const facets = props.facets ?? NO_FACETS;
  const { sessions, all, places, isPending, error } = useConsoleSessions(facets);
  const members = useMembers();
  if (error !== null) {
    return (
      <p role="alert" className="m-3 rounded-md bg-risk-soft px-3 py-2 text-sm text-risk">
        {error instanceof ApiError ? `Could not load sessions: ${error.message}` : 'Could not load sessions.'}
      </p>
    );
  }
  if (isPending) {
    return <p className="p-6 text-sm text-ink-2">Loading sessions…</p>;
  }
  return (
    <>
    {linking !== undefined && <LinkSessionDialog key={linking.id} session={linking} onClose={() => setLinking(undefined)} />}
    <SessionListView
      sessions={sessions}
      places={places}
      members={members.data}
      selectedId={props.selectedId}
      onSelect={props.onSelect}
      onLink={setLinking}
      onOpen={props.onOpen}
      onActiveChange={props.onActiveChange}
      empty={all.length === 0 ? 'No sessions yet.' : 'No sessions match these filters.'}
      {...(props.className === undefined ? {} : { className: props.className })}
    />
    </>
  );
}
