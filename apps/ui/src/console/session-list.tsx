// The session list: every session, grouped by project and workstream plus Unsorted, virtualised.
// Arrow keys, Home, End and Page keys move through sessions; Enter or a click selects one.

import { useVirtualizer } from '@tanstack/react-virtual';
import { useMemo, useRef, useState, type KeyboardEvent } from 'react';
import { ApiError, useMembers, type Member, type Session } from '../data/index.ts';
import { StatusPill } from '../design/index.ts';
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

export interface SessionListViewProps {
  sessions: readonly Session[];
  places: SessionPlaces;
  members?: readonly Member[] | undefined;
  selectedId?: string | undefined;
  onSelect: (session: Session) => void;
  /** Shown when there are no sessions. */
  empty?: string;
  className?: string;
  'aria-label'?: string;
}

/** The list itself, from data it is given. `SessionList` feeds it from the hub. */
export function SessionListView(props: SessionListViewProps) {
  'use no memo'; // TanStack Virtual's instance changes under the React Compiler's memoisation.
  const { sessions, places, selectedId, onSelect } = props;
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
  const sessionIndexes = useMemo(
    () => rows.flatMap((row, i) => (row.type === 'session' ? [i] : [])),
    [rows],
  );
  const activeRow = active === undefined ? -1 : rows.findIndex((r) => r.type === 'session' && r.session.id === active);
  const activeId = activeRow === -1 ? undefined : active;

  const moveTo = (index: number | undefined) => {
    const row = index === undefined ? undefined : rows[index];
    if (index === undefined || row?.type !== 'session') return;
    setActive(row.session.id);
    virtualizer.scrollToIndex(index, { align: 'auto' });
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
        if (row?.type === 'session') onSelect(row.session);
        break;
      }
      default:
        return;
    }
    event.preventDefault();
  };

  if (rows.length === 0) {
    return (
      <div className={cx('flex h-full items-center justify-center p-6 text-sm text-muted', props.className)}>
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
          if (activeId === undefined) moveTo(sessionIndexes.find((i) => rows[i]?.key === selectedId) ?? sessionIndexes[0]);
        }}
        className="relative w-full outline-none focus-visible:ring-2 focus-visible:ring-accent focus-visible:ring-inset"
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
                  now={now}
                  selected={row.session.id === selectedId}
                  active={row.session.id === activeId}
                  onClick={() => {
                    setActive(row.session.id);
                    onSelect(row.session);
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
      <span className="font-normal text-muted">{row.count}</span>
    </div>
  ) : (
    <div className="flex h-full items-center gap-2 pr-3 pl-5 text-xs text-muted">
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
}) {
  const { session } = props;
  const starting = session.state === 'starting';
  const state = STATE[session.state];
  return (
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
        props.active && 'ring-2 ring-accent ring-inset',
      )}
    >
      <div className="flex min-w-0 items-center gap-2">
        <span className="shrink-0 font-mono text-[10px] tracking-wide text-muted uppercase">
          {ENGINE_LABEL[session.engine]}
        </span>
        <span className={cx('min-w-0 flex-1 truncate text-sm font-medium', session.state === 'ended' && 'text-ink-2')}>
          {sessionTitle(session)}
        </span>
        <time dateTime={new Date(session.last_activity).toISOString()} className="shrink-0 text-xs text-muted">
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
              <span className="min-w-0 truncate text-muted">{session.status_line}</span>
            )}
          </>
        )}
      </div>
    </div>
  );
}

export interface SessionListProps {
  facets?: SessionFacets;
  selectedId?: string | undefined;
  onSelect: (session: Session) => void;
  className?: string;
}

/** The session list, live from the hub, filtered by `facets`. */
export function SessionList(props: SessionListProps) {
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
    return <p className="p-6 text-sm text-muted">Loading sessions…</p>;
  }
  return (
    <SessionListView
      sessions={sessions}
      places={places}
      members={members.data}
      selectedId={props.selectedId}
      onSelect={props.onSelect}
      empty={all.length === 0 ? 'No sessions yet.' : 'No sessions match these filters.'}
      {...(props.className === undefined ? {} : { className: props.className })}
    />
  );
}
