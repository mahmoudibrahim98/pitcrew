// The bar above the page: the layout switcher (top left, Ctrl .), the breadcrumb, the stream's
// state when it is not live, search (Ctrl K), "+ New" and the Orchestrator toggle (Ctrl J).

import { Link, useParams, useRouter, useRouterState } from '@tanstack/react-router';
import { ToggleGroup } from 'radix-ui';
import {
  useConnection,
  useGatewayWorkspace,
  useProjects,
  useSessions,
  useTasks,
  useWorkspace,
  useWorkstreams,
} from '../data/index.ts';
import { Button, FOCUS_RING, Kbd, SearchIcon, SparkleIcon, StatusPill, Tooltip } from '../design/index.ts';
import { cx } from '../lib/cx.ts';
import { ariaShortcut } from '../lib/platform.ts';
import { WorkspaceName, workspaceLabel } from './workspace-name.tsx';
import { NewMenu } from './create.tsx';
import type { LayoutId } from './feature.ts';
import { LAYOUTS, switchLayout, useLayout, useWorkspaceId } from './layout.ts';
import { paths } from './paths.ts';
import { SHORTCUTS } from './shortcuts.ts';
import { useShell } from './store.ts';

function LayoutSwitcher() {
  const router = useRouter();
  const ws = useWorkspaceId();
  const layout = useLayout();
  return (
    <Tooltip content="Switch layout" keys={SHORTCUTS.layout}>
      <ToggleGroup.Root
        type="single"
        value={layout}
        onValueChange={(value) => {
          if (value !== '') switchLayout(router, ws, value as LayoutId);
        }}
        aria-label="Layout"
        aria-keyshortcuts={ariaShortcut(SHORTCUTS.layout)}
        className="inline-flex shrink-0 rounded-md border border-line-2 bg-sunken p-0.5"
      >
        {(Object.keys(LAYOUTS) as LayoutId[]).map((id) => (
          <ToggleGroup.Item
            key={id}
            value={id}
            className={cx(
              'h-6 rounded-sm px-2.5 text-sm text-ink-2 outline-none hover:text-ink data-[state=on]:bg-card data-[state=on]:font-medium data-[state=on]:text-ink data-[state=on]:shadow-[0_0_0_1px_var(--pc-line)]',
              FOCUS_RING,
            )}
          >
            {LAYOUTS[id].label}
          </ToggleGroup.Item>
        ))}
      </ToggleGroup.Root>
    </Tooltip>
  );
}

interface Crumb {
  label: string;
  host?: string | undefined;
  to?: string;
}

function useCrumbs(): Crumb[] {
  const ws = useWorkspaceId();
  const params: { project?: string; workstream?: string; task?: string; session?: string } = useParams({
    strict: false,
  });
  const title = useRouterState({
    select: (s) => s.matches.findLast((m) => m.staticData?.title !== undefined)?.staticData.title,
  });
  const workspace = useWorkspace().data?.workspace;
  // In the desktop app the gateway knows the name even while the daemon cannot be reached.
  const gateway = useGatewayWorkspace();
  const projects = useProjects().data;
  const workstreams = useWorkstreams().data;
  const tasks = useTasks().data;
  const sessions = useSessions().data;

  const crumbs: Crumb[] = [{ label: gateway?.name ?? workspace?.name ?? 'Workspace', host: gateway?.kind === 'remote' ? gateway.host : undefined, to: paths.workspace(ws) }];
  const workstream = workstreams?.find((w) => w.id === params.workstream);
  const task = tasks?.find((t) => t.key === params.task || t.id === params.task);
  const projectId = params.project ?? workstream?.project ?? task?.project;
  const project = projects?.find((p) => p.id === projectId);
  if (project !== undefined) crumbs.push({ label: project.name, to: paths.project(ws, project.id) });
  if (workstream !== undefined) {
    crumbs.push({ label: workstream.name, to: paths.workstream(ws, workstream.project, workstream.id) });
  }
  if (task !== undefined) crumbs.push({ label: task.key, to: paths.task(ws, task.key) });
  if (params.session !== undefined) {
    const session = sessions?.find((s) => s.id === params.session);
    crumbs.push({ label: LAYOUTS.console.label, to: paths.console(ws) });
    crumbs.push({ label: session?.title ?? 'Session' });
  } else if (title !== undefined && crumbs.length === 1) {
    crumbs.push({ label: title });
  }
  return crumbs;
}

/** A separator between two crumbs. */
function Sep() {
  return (
    <span aria-hidden className="shrink-0 text-line-2">
      /
    </span>
  );
}

function CrumbLink({ crumb, last }: { crumb: Crumb; last: boolean }) {
  if (last || crumb.to === undefined) {
    return (
      <span aria-current={last ? 'page' : undefined} className={cx('truncate', last ? 'font-medium text-ink' : 'text-ink-2')}>
        <WorkspaceName workspace={{ name: crumb.label, kind: 'remote', host: crumb.host }} />
      </span>
    );
  }
  return (
    <Link to={crumb.to} activeOptions={{ exact: true }} className="truncate text-ink-2 hover:text-ink hover:underline">
      <WorkspaceName workspace={{ name: crumb.label, kind: 'remote', host: crumb.host }} />
    </Link>
  );
}

/**
 * Everything before the current page, once there is no room to show it: a single marker that
 * keeps those segments reachable by name (hover, or keyboard focus) instead of truncating one of
 * them into something unreadable.
 */
function CollapsedCrumbs({ labels }: { labels: readonly string[] }) {
  const text = labels.join(' / ');
  return (
    <Tooltip content={text} side="bottom">
      <button
        type="button"
        aria-label={`Collapsed: ${text}`}
        // The "…" is drawn with CSS, not as text: a display:none element's text still counts
        // toward its ancestors' textContent, which would otherwise leak into it even while hidden.
        className={cx(
          "flex size-5 shrink-0 items-center justify-center rounded-sm text-ink-2 outline-none before:content-['…'] hover:bg-hover hover:text-ink @4xl:hidden",
          FOCUS_RING,
        )}
      />
    </Tooltip>
  );
}

function Breadcrumb() {
  const crumbs = useCrumbs();
  const last = crumbs[crumbs.length - 1];
  // Collapsed first, not truncated: everything before the current page gives way before it does,
  // so the page on screen stays readable instead of being crushed to a sliver alongside it.
  const rest = crumbs.slice(0, -1);
  const fullPath = crumbs.map((c) => workspaceLabel({ name: c.label, kind: 'remote', host: c.host })).join(' / ');
  if (last === undefined) return <nav aria-label="Breadcrumb" className="min-w-0 flex-1" />;

  return (
    <nav aria-label="Breadcrumb" className="min-w-0 flex-1">
      {/* The full path stays available to assistive tech even once the lead-up collapses visually. */}
      <ol className="flex min-w-0 items-center gap-1.5 text-sm" aria-label={rest.length > 0 ? fullPath : undefined}>
        {rest.length > 0 && (
          <li className="flex min-w-0 shrink items-center gap-1.5">
            {/* The full trail once there is room. */}
            <span className="hidden min-w-0 items-center gap-1.5 @4xl:flex">
              {rest.map((crumb, i) => (
                <span key={`${i}-$<WorkspaceName workspace={{ name: crumb.label, kind: 'remote', host: crumb.host }} />`} className="flex min-w-0 max-w-48 items-center gap-1.5">
                  {i > 0 && <Sep />}
                  <CrumbLink crumb={crumb} last={false} />
                </span>
              ))}
            </span>
            {/* Collapsed below that: still reachable by name, not gone. */}
            <CollapsedCrumbs labels={rest.map((c) => workspaceLabel({ name: c.label, kind: 'remote', host: c.host }))} />
          </li>
        )}
        <li className="flex min-w-0 shrink items-center gap-1.5">
          {rest.length > 0 && <Sep />}
          <CrumbLink crumb={last} last />
        </li>
      </ol>
    </nav>
  );
}

function StreamState() {
  const { status, problem } = useConnection();
  // In the desktop app the gateway says what it knows first.
  const gateway = useGatewayWorkspace()?.state;
  let pill = null;
  if (gateway === 'needs_pairing' || problem === 'needs_pairing') pill = <StatusPill tone="risk">Needs pairing</StatusPill>;
  else if (gateway === 'unreachable') pill = <StatusPill tone="risk">Unreachable</StatusPill>;
  // The gateway is still connecting (an SSH tunnel, say): the stream's failures are expected.
  else if (gateway === 'connecting' && status !== 'live') pill = <StatusPill tone="accent">Connecting</StatusPill>;
  else if (problem === 'unauthorized') pill = <StatusPill tone="risk">Token rejected</StatusPill>;
  else if (problem === 'unreachable') pill = <StatusPill tone="risk">Hub unreachable</StatusPill>;
  else if (status === 'connecting') pill = <StatusPill tone="accent">Connecting</StatusPill>;
  else if (status === 'reconnecting') pill = <StatusPill tone="warn">Reconnecting</StatusPill>;
  else if (status === 'stopped') pill = <StatusPill tone="neutral">Offline</StatusPill>;
  return (
    <span role="status" data-testid="stream-status" data-status={status} className="inline-flex">
      {pill}
    </span>
  );
}

export function TopBar() {
  const paletteOpen = useShell((s) => s.paletteOpen);
  const setPaletteOpen = useShell((s) => s.setPaletteOpen);
  const orchestratorOpen = useShell((s) => s.orchestratorOpen);
  const setOrchestratorOpen = useShell((s) => s.setOrchestratorOpen);
  // On a narrow bar the shortcut hints go first, then the button labels (kept for screen readers).
  return (
    <header className="@container flex h-12 shrink-0 items-center gap-3 border-b border-line bg-bg px-3">
      <LayoutSwitcher />
      <Breadcrumb />
      <div className="ml-auto flex shrink-0 items-center gap-1.5">
        <StreamState />
        <Tooltip content="Search and commands" keys={SHORTCUTS.palette}>
          <Button
            aria-haspopup="dialog"
            aria-expanded={paletteOpen}
            aria-keyshortcuts={ariaShortcut(SHORTCUTS.palette)}
            onClick={() => setPaletteOpen(true)}
            className="text-ink-2"
          >
            <SearchIcon />
            <span className="sr-only @4xl:not-sr-only">Search</span>
            <span className="hidden @5xl:inline-flex">
              <Kbd keys={SHORTCUTS.palette} />
            </span>
          </Button>
        </Tooltip>
        <NewMenu />
        <Tooltip content="The Orchestrator panel" keys={SHORTCUTS.orchestrator}>
          <Button
            aria-pressed={orchestratorOpen}
            aria-keyshortcuts={ariaShortcut(SHORTCUTS.orchestrator)}
            onClick={() => setOrchestratorOpen(!orchestratorOpen)}
            className="aria-pressed:border-accent aria-pressed:bg-accent-soft aria-pressed:text-accent-text"
          >
            <SparkleIcon />
            <span className="sr-only @4xl:not-sr-only">Orchestrator</span>
            <span className="hidden @5xl:inline-flex">
              <Kbd keys={SHORTCUTS.orchestrator} />
            </span>
          </Button>
        </Tooltip>
      </div>
    </header>
  );
}
