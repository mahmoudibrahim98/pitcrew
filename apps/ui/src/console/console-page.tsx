// The Agent console: the filters, the session list and the chosen session (header, chat and
// composer) side by side, each side pane resizable and remembered; in a narrow console, one pane
// at a time with a way back. The chosen session is in the path and the filters in the search
// (`/w/$ws/console/$session?state=waiting`), so a link or a reload reproduces the view.
//
// Keys: F6 and Shift+F6 move between the panes (filters, list, transcript, composer, or the
// terminal or work view in their place). In the list, the arrow keys choose the session shown
// beside it; Enter opens it and goes to the composer. A terminal in control mode keeps F6 for its
// program.
//
// The session area is the workbench (`workbench/`): sessions, terminals and files in tabs and
// splits, kept per workspace. The chosen session in the URL is revealed there (a session chosen
// from the list opens as the active pane's preview tab), and the tab the person brings on screen
// is put back in the URL. A session tab shows the chat, its terminal when the session has one
// (`?view=terminal`), or its work (`?view=work`, src/projects' `SessionWork`, lazy). In a narrow
// console the workbench gives way to the one session in the URL, as before.

import { defaultStringifySearch, useParams, useRouter, useSearch } from '@tanstack/react-router';
import {
  useEffect,
  useEffectEvent,
  useLayoutEffect,
  useRef,
  useState,
  type KeyboardEvent,
  type ReactNode,
  type RefObject,
} from 'react';
import type { Session } from '../data/index.ts';
import { Badge, Button, ChevronRightIcon, Kbd, ResizablePanel } from '../design/index.ts';
import { paths, SHELL_KEYS_ATTRIBUTE, useWorkspaceId } from '../shell/index.ts';
import { StartSessionButton } from './start-session-button.tsx';
import { useConsoleSessions } from './data.ts';
import { NO_FACETS, type SessionFacets } from './facets.ts';
import { onIntent, takeIntent, type ConsoleIntent } from './intent.ts';
import { NARROW_BELOW, PANE_WIDTH, usePanes } from './panes.ts';
import {
  facetCount,
  facetsFromSearch,
  searchWithFacets,
  searchWithView,
  viewFromSearch,
  type SessionView,
} from './search.ts';
import { SessionFilters } from './session-filters.tsx';
import { SessionList, type OpenWhere, type SelectVia } from './session-list.tsx';
import { currentTab, keep, openTab, PANE_MIN_WIDTH, reveal, type TabRef } from './workbench/layout.ts';
import { Placeholder, SessionPane } from './workbench/session-tab.tsx';
import { useWorkbench, Workbench } from './workbench/workbench.tsx';

/** How long the arrow keys must rest on a session before the session pane follows. */
const FOLLOW_MS = 150;

type Pane = 'filters' | 'list' | 'terminal' | 'work' | 'chat' | 'composer' | 'file' | 'details';

/** What F6 focuses in each pane (`[data-pane]`), if the pane is on screen and has it. */
const FOCUS_IN: Record<Pane, string> = {
  filters: 'input:not(:disabled)',
  list: '[role="listbox"]',
  terminal: '[data-terminal-focus]',
  work: '[data-work-focus]',
  chat: '[data-chat-scroller]',
  composer: 'textarea:not(:disabled)',
  file: '[data-file-focus]',
  details: '[data-details-focus]',
};

const PANE_FOCUS = Object.fromEntries(
  Object.entries(FOCUS_IN).map(([pane, inner]) => [pane, `[data-pane="${pane}"] ${inner}`]),
) as Record<Pane, string>;

/** A workbench pane's content, where "next pane" from the palette starts. */
const CONTENT_FOCUS = '[data-chat-scroller], [data-terminal-focus], [data-work-focus], [data-file-focus]';

/** F6's stops, in the order they are on screen: the filters, the list, each pane's, the details. */
function paneStops(root: HTMLElement | null): { pane: HTMLElement; target: HTMLElement }[] {
  if (root === null) return [];
  return [...root.querySelectorAll<HTMLElement>('[data-pane]')].flatMap((pane) => {
    const inner = FOCUS_IN[pane.dataset.pane as Pane] as string | undefined;
    const target = inner === undefined ? null : pane.querySelector<HTMLElement>(inner);
    return target === null ? [] : [{ pane, target }];
  });
}

const find = (root: HTMLElement | null, selector: string) => root?.querySelector<HTMLElement>(selector) ?? null;

/** Whether the console is narrower than `NARROW_BELOW`: one pane at a time. */
function useNarrow(root: RefObject<HTMLElement | null>): { narrow: boolean; width: number } {
  const [width, setWidth] = useState(0);
  useLayoutEffect(() => {
    const element = root.current;
    if (element === null) return;
    // A width of 0 is no layout at all (a hidden tab, a test), not a narrow console.
    let below = false;
    const measure = () => {
      const width = element.clientWidth;
      setWidth(width);
      const compact = width > 0 && width < 960;
      if (compact && !below) usePanes.getState().setFiltersOpen(false);
      below = compact;
    };
    // Measured before the first paint, so a narrow window does not flash the wide layout.
    measure();
    if (typeof ResizeObserver !== 'function') return;
    const observer = new ResizeObserver(measure);
    observer.observe(element);
    return () => observer.disconnect();
  }, [root]);
  return { narrow: width > 0 && width < NARROW_BELOW, width };
}

/** Focus is nowhere in particular: on the page itself, as a closing palette or dialog leaves it. */
const adrift = () => {
  const active = document.activeElement;
  return active === null || active === document.body || active.tagName === 'MAIN';
};

/**
 * Focuses an element that may not be on screen yet (a pane still loading), retrying for a while.
 * The palette hands focus back to the page as it closes, which can come after a command has run,
 * so for two seconds focus that drifts back to the page is put back; focus moved anywhere else
 * stays where it was moved.
 */
function useFocusSoon(): (target: () => HTMLElement | null, fallback?: () => HTMLElement | null) => void {
  const timer = useRef<number | undefined>(undefined);
  useEffect(() => () => window.clearTimeout(timer.current), []);
  return (target, fallback) => {
    window.clearTimeout(timer.current);
    let tries = 0;
    let placed: HTMLElement | null = null;
    const attempt = () => {
      tries += 1;
      if (placed === null) {
        placed = target() ?? (tries >= 12 ? (fallback?.() ?? null) : null);
        placed?.focus();
      } else if (adrift()) {
        // The element may have been drawn afresh since (the list as its filters change).
        if (!placed.isConnected) placed = target() ?? placed;
        placed.focus();
      }
      if (tries < 40) timer.current = window.setTimeout(attempt, 50);
    };
    timer.current = window.setTimeout(attempt, 50);
  };
}

export function ConsolePage() {
  const ws = useWorkspaceId();
  const router = useRouter();
  const { session: sessionId }: { session?: string } = useParams({ strict: false });
  const search: Record<string, unknown> = useSearch({ strict: false });
  const facets = facetsFromSearch(search);
  const { sessions, all } = useConsoleSessions(facets);
  const root = useRef<HTMLDivElement>(null);
  const measured = useNarrow(root);
  const width = measured.width;
  const panes = usePanes();
  const narrow = measured.narrow || (panes.filtersOpen && width > 0 && width < PANE_WIDTH.filters.min + PANE_WIDTH.list.min + PANE_MIN_WIDTH);
  const filterMax = width > 0 ? Math.max(PANE_WIDTH.filters.min, width - PANE_WIDTH.list.min - PANE_MIN_WIDTH) : PANE_WIDTH.filters.max;
  const filterWidth = Math.min(panes.filtersWidth, filterMax);
  const listMax = width > 0 ? Math.max(PANE_WIDTH.list.min, width - PANE_MIN_WIDTH - (panes.filtersOpen ? filterWidth : 0)) : PANE_WIDTH.list.max;
  const [narrowFilters, setNarrowFilters] = useState(false);
  const focusSoon = useFocusSoon();

  const stringify = router.options.stringifySearch ?? defaultStringifySearch;
  /** Goes to the console with another session or other filters; the rest of the view stays. */
  const go = (to: { session?: string | undefined; facets?: SessionFacets }, replace = false) => {
    const session = 'session' in to ? to.session : sessionId;
    const path = session === undefined ? paths.console(ws) : paths.session(ws, session);
    const next = to.facets === undefined ? search : searchWithFacets(search, to.facets);
    void router.navigate({ href: path + stringify(next), replace });
  };
  const setFacets = (next: SessionFacets) => go({ facets: next }, true);
  const view = viewFromSearch(search);
  /** Chat or terminal, in the URL; a switch, not a step, so it replaces the history entry. */
  const setView = (next: SessionView) => {
    if (sessionId === undefined) return;
    void router.navigate({ href: paths.session(ws, sessionId) + stringify(searchWithView(search, next)), replace: true });
  };

  /**
   * Puts the tab on screen in the URL: a session with its view, or the bare console for a file or
   * nothing. Switching tabs replaces the history entry; choosing from the list adds one.
   */
  const show = (ref: TabRef | undefined, replace = true) => {
    const href =
      ref?.kind === 'session'
        ? paths.session(ws, ref.session) + stringify(searchWithView(search, ref.view))
        : paths.console(ws) + stringify(searchWithView(search, 'chat'));
    if (href !== router.state.location.href) void router.navigate({ href, replace });
  };
  const workbench = useWorkbench(ws, show);

  // A link, the palette or the history asks for a session (in a view): the workbench shows it.
  const revealAsked = useEffectEvent((id: string, asked: SessionView) => {
    workbench.store.update((l) => reveal(l, { kind: 'session', session: id, view: asked }, { preview: true }));
  });
  useEffect(() => {
    if (!narrow && sessionId !== undefined) revealAsked(sessionId, view);
  }, [narrow, sessionId, view]);

  /**
   * A session chosen in the list: shown where it is open already (in whatever view it has there),
   * or opened in the active pane as its preview tab. Enter keeps it.
   */
  const openFromList = (session: Session, how: SelectVia | 'arrows') => {
    if (narrow) {
      if (session.id !== sessionId) go({ session: session.id }, how === 'arrows');
      return;
    }
    const next = workbench.store.update((l) => {
      const shown = reveal(l, { kind: 'session', session: session.id, view }, { preview: how !== 'keyboard', anyView: true });
      const now = currentTab(shown);
      return how === 'keyboard' && now !== undefined ? keep(shown, now.group.id, now.tab.id) : shown;
    });
    show(currentTab(next)?.tab.ref, how === 'arrows');
  };
  /** "Open in a new tab" and "Open to the side", from a row's menu. */
  const openKept = (session: Session, where: OpenWhere) => {
    if (narrow) {
      go({ session: session.id });
      return;
    }
    const ref: TabRef = { kind: 'session', session: session.id, view: 'chat' };
    workbench.change((l) => openTab(l, ref, where === 'side' ? { side: 'right' } : {}), { focus: 'tab' });
  };

  // The arrow keys choose the session beside the list, once they rest on one.
  const followTimer = useRef<number | undefined>(undefined);
  useEffect(() => () => window.clearTimeout(followTimer.current), []);
  const follow = (session: Session) => {
    window.clearTimeout(followTimer.current);
    followTimer.current = window.setTimeout(() => openFromList(session, 'arrows'), FOLLOW_MS);
  };
  /** The active pane's content, or the whole console when there are no panes (narrow). */
  const activeArea = () => root.current?.querySelector<HTMLElement>('[data-active-group]') ?? root.current;
  const focusComposer = () =>
    focusSoon(
      () => find(activeArea(), PANE_FOCUS.composer),
      () => find(activeArea(), PANE_FOCUS.chat),
    );
  const select = (session: Session, via: SelectVia) => {
    window.clearTimeout(followTimer.current);
    openFromList(session, via);
    if (via === 'keyboard') focusComposer();
  };

  /** F6: the next (or previous) pane on screen, from the one with focus. */
  const stepPane = (step: 1 | -1, from: Element | null) => {
    const stops = paneStops(root.current);
    if (stops.length === 0) return undefined;
    const here = from?.closest('[data-pane]');
    const at = stops.findIndex((stop) => stop.pane === here);
    const next = at === -1 ? (step === 1 ? 0 : stops.length - 1) : (at + step + stops.length) % stops.length;
    return stops[next]?.target;
  };

  const showFilters = (open: boolean) => (narrow ? setNarrowFilters(open) : panes.setFiltersOpen(open));
  const filtersShown = narrow ? narrowFilters : panes.filtersOpen;

  // Requests from the palette (see intent.ts): this page may have just mounted for one.
  const apply = (intent: ConsoleIntent) => {
    if (intent.kind === 'workbench') {
      const { action } = intent;
      if (action === 'next-pane' || action === 'previous-pane') {
        // From the active pane: the palette had focus, not a pane.
        const from = find(activeArea(), CONTENT_FOCUS) ?? activeArea();
        const target = stepPane(action === 'next-pane' ? 1 : -1, from);
        if (target !== undefined) focusSoon(() => target);
        return;
      }
      if (narrow) return;
      workbench.run(action);
      const open = workbench.store.get().details.open;
      focusSoon(() =>
        action === 'toggle-details' && open
          ? find(root.current, PANE_FOCUS.details)
          : find(activeArea(), '[role="tab"][aria-selected="true"]'),
      );
      return;
    }
    const toList = narrow && sessionId !== undefined;
    if (intent.kind === 'facet') {
      showFilters(true);
      focusSoon(() => find(root.current, `[data-facet="${intent.facet}"] input`));
      return;
    }
    if (narrow) setNarrowFilters(false);
    if (intent.kind === 'state' || intent.kind === 'clear') {
      const next = intent.kind === 'clear' ? NO_FACETS : { ...facets, state: [intent.state] };
      go(toList ? { session: undefined, facets: next } : { facets: next }, !toList);
    } else if (toList) {
      go({ session: undefined });
    }
    focusSoon(() => find(root.current, PANE_FOCUS.list));
  };
  const takeRequest = useEffectEvent(() => {
    const intent = takeIntent();
    if (intent !== undefined) apply(intent);
  });
  useEffect(() => {
    // A tick after mounting, so that a mount React undoes at once (Strict Mode, in development)
    // leaves the request to the mount that stays.
    const timer = window.setTimeout(() => takeRequest(), 0);
    const off = onIntent(() => takeRequest());
    return () => {
      window.clearTimeout(timer);
      off();
    };
  }, []);

  const onKeyDown = (event: KeyboardEvent<HTMLDivElement>) => {
    if (event.key !== 'F6' || event.ctrlKey || event.metaKey || event.altKey) return;
    // A surface that owns the keys (a terminal in control mode) keeps F6 for its program.
    if (event.target instanceof Element && event.target.closest(`[${SHELL_KEYS_ATTRIBUTE}="none"]`) !== null) return;
    const target = stepPane(event.shiftKey ? -1 : 1, document.activeElement);
    if (target === undefined) return;
    event.preventDefault();
    target.focus();
  };

  const active = facetCount(facets);
  const filters = (
    <div data-pane="filters" className="min-h-0 flex-1 overflow-y-auto">
      <SessionFilters value={facets} onChange={setFacets} />
    </div>
  );
  const list = (
    <div data-pane="list" className="flex min-h-0 flex-1 flex-col">
      <div className="flex h-11 shrink-0 items-center gap-2 border-b border-line pr-2 pl-3">
        <h1 className="text-md font-semibold whitespace-nowrap">Agent console</h1>
        <span className="truncate text-xs text-ink-2 tabular-nums" data-testid="session-count">
          {active > 0 ? `${sessions.length} of ${all.length}` : all.length} {all.length === 1 ? 'session' : 'sessions'}
        </span>
        <StartSessionButton />
        <Button variant="ghost" className="ml-auto" aria-pressed={filtersShown} onClick={() => showFilters(!filtersShown)}>
          Filters
          {active > 0 && (
            <Badge tone="accent" label={active === 1 ? 'filter on' : 'filters on'}>
              {active}
            </Badge>
          )}
        </Button>
      </div>
      <div className="min-h-0 flex-1">
        <SessionList
          facets={facets}
          selectedId={sessionId}
          onSelect={select}
          onOpen={openKept}
          onActiveChange={narrow ? undefined : follow}
        />
      </div>
    </div>
  );
  const choose = (
    <Placeholder title="Choose a session">
      <p>
        Pick one from the list to read its chat and talk to its agent. <Kbd keys={['F6']} /> moves between panes.
      </p>
    </Placeholder>
  );

  if (narrow) {
    const back = (label: string, onClick: () => void) => (
      <div className="flex h-11 shrink-0 items-center gap-2 border-b border-line px-2">
        <Button variant="ghost" onClick={onClick}>
          <ChevronRightIcon className="rotate-180" />
          {label}
        </Button>
        <h1 className="sr-only">Agent console</h1>
      </div>
    );
    let pane: ReactNode;
    if (narrowFilters) {
      pane = (
        <section aria-label="Filters" className="flex min-h-0 w-full flex-col">
          {back('Sessions', () => setNarrowFilters(false))}
          {filters}
        </section>
      );
    } else if (sessionId !== undefined) {
      pane = (
        <section aria-label="Session" className="flex min-h-0 w-full min-w-0 flex-col">
          {back('Sessions', () => go({ session: undefined }))}
          {sessionId === undefined ? (
            choose
          ) : (
            <SessionPane key={sessionId} ws={ws} sessionId={sessionId} view={view} onView={setView} narrow />
          )}
        </section>
      );
    } else {
      pane = (
        <section aria-label="Sessions" className="flex min-h-0 w-full flex-col">
          {list}
        </section>
      );
    }
    return (
      <div ref={root} data-console-layout="narrow" onKeyDown={onKeyDown} className="flex h-full min-h-0 overflow-hidden">
        {pane}
      </div>
    );
  }

  return (
    <div ref={root} data-console-layout="wide" onKeyDown={onKeyDown} className="flex h-full min-h-0 overflow-hidden">
      {panes.filtersOpen && (
        <ResizablePanel
          as="section"
          side="left"
          label="Filters"
          width={filterWidth}
          onWidthChange={panes.setFiltersWidth}
          min={PANE_WIDTH.filters.min}
          max={Math.min(PANE_WIDTH.filters.max, filterMax)}
          className="border-r border-line bg-sidebar"
        >
          {filters}
        </ResizablePanel>
      )}
      <ResizablePanel
        as="section"
        side="left"
        label="Sessions"
        width={Math.min(panes.listWidth, listMax)}
        onWidthChange={panes.setListWidth}
        min={PANE_WIDTH.list.min}
        max={Math.min(PANE_WIDTH.list.max, listMax)}
        className="border-r border-line"
      >
        {list}
      </ResizablePanel>
      <Workbench api={workbench} empty={choose} />
    </div>
  );
}
