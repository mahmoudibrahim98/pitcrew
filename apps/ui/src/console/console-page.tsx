// The Agent console: the filters, the session list and the chosen session (header, chat and
// composer) side by side, each side pane resizable and remembered; in a narrow console, one pane
// at a time with a way back. The chosen session is in the path and the filters in the search
// (`/w/$ws/console/$session?state=waiting`), so a link or a reload reproduces the view.
//
// Keys: F6 and Shift+F6 move between the panes (filters, list, transcript, composer). In the list,
// the arrow keys choose the session shown beside it; Enter opens it and goes to the composer.

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
import { ApiError, useSession, type Session } from '../data/index.ts';
import { Badge, Button, ChevronRightIcon, ConsoleIcon, Kbd, ResizablePanel } from '../design/index.ts';
import { paths, useWorkspaceId } from '../shell/index.ts';
import { ChatView } from './chat-view.tsx';
import { Composer } from './composer.tsx';
import { useConsoleSessions } from './data.ts';
import { NO_FACETS, type SessionFacets } from './facets.ts';
import { onIntent, takeIntent, type ConsoleIntent } from './intent.ts';
import { NARROW_BELOW, PANE_WIDTH, usePanes } from './panes.ts';
import { facetCount, facetsFromSearch, searchWithFacets } from './search.ts';
import { SessionFilters } from './session-filters.tsx';
import { SessionHeader } from './session-header.tsx';
import { SessionList, type SelectVia } from './session-list.tsx';

/** How long the arrow keys must rest on a session before the session pane follows. */
const FOLLOW_MS = 150;

type Pane = 'filters' | 'list' | 'chat' | 'composer';

const PANE_ORDER: readonly Pane[] = ['filters', 'list', 'chat', 'composer'];

/** What F6 focuses in each pane, if the pane is on screen and has it. */
const PANE_FOCUS: Record<Pane, string> = {
  filters: '[data-pane="filters"] input:not(:disabled)',
  list: '[data-pane="list"] [role="listbox"]',
  chat: '[data-pane="chat"] [data-chat-scroller]',
  composer: '[data-pane="composer"] textarea:not(:disabled)',
};

const find = (root: HTMLElement | null, selector: string) => root?.querySelector<HTMLElement>(selector) ?? null;

/** Whether the console is narrower than `NARROW_BELOW`: one pane at a time. */
function useNarrow(root: RefObject<HTMLElement | null>): boolean {
  const [narrow, setNarrow] = useState(false);
  useLayoutEffect(() => {
    const element = root.current;
    if (element === null) return;
    // A width of 0 is no layout at all (a hidden tab, a test), not a narrow console.
    const measure = () => {
      const width = element.clientWidth;
      setNarrow(width > 0 && width < NARROW_BELOW);
    };
    // Measured before the first paint, so a narrow window does not flash the wide layout.
    measure();
    if (typeof ResizeObserver !== 'function') return;
    const observer = new ResizeObserver(measure);
    observer.observe(element);
    return () => observer.disconnect();
  }, [root]);
  return narrow;
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
  const narrow = useNarrow(root);
  const panes = usePanes();
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

  // The arrow keys choose the session beside the list, once they rest on one.
  const followTimer = useRef<number | undefined>(undefined);
  useEffect(() => () => window.clearTimeout(followTimer.current), []);
  const follow = (session: Session) => {
    window.clearTimeout(followTimer.current);
    followTimer.current = window.setTimeout(() => go({ session: session.id }, true), FOLLOW_MS);
  };
  const focusComposer = () =>
    focusSoon(
      () => find(root.current, PANE_FOCUS.composer),
      () => find(root.current, PANE_FOCUS.chat),
    );
  const select = (session: Session, via: SelectVia) => {
    window.clearTimeout(followTimer.current);
    if (session.id !== sessionId) go({ session: session.id });
    if (via === 'keyboard') focusComposer();
  };

  const showFilters = (open: boolean) => (narrow ? setNarrowFilters(open) : panes.setFiltersOpen(open));
  const filtersShown = narrow ? narrowFilters : panes.filtersOpen;

  // Requests from the palette (see intent.ts): this page may have just mounted for one.
  const apply = (intent: ConsoleIntent) => {
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
    const available = PANE_ORDER.filter((pane) => find(root.current, PANE_FOCUS[pane]) !== null);
    if (available.length === 0) return;
    event.preventDefault();
    const here = document.activeElement?.closest('[data-pane]')?.getAttribute('data-pane');
    const at = available.indexOf(here as Pane);
    const step = event.shiftKey ? -1 : 1;
    const next = at === -1 ? (event.shiftKey ? available.length - 1 : 0) : (at + step + available.length) % available.length;
    const pane = available[next];
    if (pane !== undefined) find(root.current, PANE_FOCUS[pane])?.focus();
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
          onActiveChange={narrow ? undefined : follow}
        />
      </div>
    </div>
  );
  const session =
    sessionId === undefined ? (
      <Placeholder title="Choose a session">
        Pick one from the list to read its chat and talk to its agent. <Kbd keys={['F6']} /> moves between panes.
      </Placeholder>
    ) : (
      <SessionPane key={sessionId} ws={ws} sessionId={sessionId} />
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
          {session}
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
          width={panes.filtersWidth}
          onWidthChange={panes.setFiltersWidth}
          min={PANE_WIDTH.filters.min}
          max={PANE_WIDTH.filters.max}
          className="border-r border-line bg-sidebar"
        >
          {filters}
        </ResizablePanel>
      )}
      <ResizablePanel
        as="section"
        side="left"
        label="Sessions"
        width={panes.listWidth}
        onWidthChange={panes.setListWidth}
        min={PANE_WIDTH.list.min}
        max={PANE_WIDTH.list.max}
        className="border-r border-line"
      >
        {list}
      </ResizablePanel>
      <section aria-label="Session" className="flex min-w-0 flex-1 flex-col">
        {session}
      </section>
    </div>
  );
}

function Placeholder({ title, children }: { title: string; children: ReactNode }) {
  return (
    <div className="flex flex-1 flex-col items-center justify-center gap-2 p-6 text-center">
      <ConsoleIcon className="size-6 text-ink-2" />
      <p className="text-sm font-medium">{title}</p>
      <p className="max-w-80 text-sm text-ink-2">{children}</p>
    </div>
  );
}

/** One session: its header (with links to its task and workstream), chat and composer. */
function SessionPane({ ws, sessionId }: { ws: string; sessionId: string }) {
  const router = useRouter();
  const session = useSession(sessionId);
  if (session.error instanceof ApiError && session.error.code === 'not_found') {
    return (
      <Placeholder title="No such session">
        It may have been in another workspace, or the link may be wrong. Choose one from the list.
      </Placeholder>
    );
  }
  const open = (href: string) => void router.navigate({ href });
  const taskHref = (task: { key: string }) => paths.task(ws, task.key);
  const workstreamHref = (w: { id: string; project: string }) => paths.workstream(ws, w.project, w.id);
  return (
    <>
      <SessionHeader
        sessionId={sessionId}
        taskHref={taskHref}
        onOpenTask={(task) => open(taskHref(task))}
        workstreamHref={workstreamHref}
        onOpenWorkstream={(w) => open(workstreamHref(w))}
      />
      <div data-pane="chat" className="min-h-0 flex-1">
        <ChatView sessionId={sessionId} />
      </div>
      <div data-pane="composer" className="shrink-0">
        <Composer sessionId={sessionId} />
      </div>
    </>
  );
}
