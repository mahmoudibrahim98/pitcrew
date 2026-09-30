// The bar above the page: the layout switcher (top left, Ctrl .), the breadcrumb, the stream's
// state when it is not live, search (Ctrl K), "+ New" and the Orchestrator toggle (Ctrl J).

import { Link, useParams, useRouter, useRouterState } from '@tanstack/react-router';
import { ToggleGroup } from 'radix-ui';
import {
  useConnection,
  useProjects,
  useSessions,
  useTasks,
  useWorkspace,
  useWorkstreams,
} from '../data/index.ts';
import { Button, Kbd, SearchIcon, SparkleIcon, StatusPill, Tooltip } from '../design/index.ts';
import { cx } from '../lib/cx.ts';
import { ariaShortcut } from '../lib/platform.ts';
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
            className="h-6 rounded-sm px-2.5 text-sm text-ink-2 outline-none hover:text-ink focus-visible:outline-2 focus-visible:outline-accent data-[state=on]:bg-card data-[state=on]:font-medium data-[state=on]:text-ink data-[state=on]:shadow-[0_0_0_1px_var(--pc-line)]"
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
  const projects = useProjects().data;
  const workstreams = useWorkstreams().data;
  const tasks = useTasks().data;
  const sessions = useSessions().data;

  const crumbs: Crumb[] = [{ label: workspace?.name ?? 'Workspace', to: paths.workspace(ws) }];
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

function Breadcrumb() {
  const crumbs = useCrumbs();
  return (
    <nav aria-label="Breadcrumb" className="min-w-0 flex-1">
      <ol className="flex min-w-0 items-center gap-1.5 text-sm">
        {crumbs.map((crumb, i) => {
          const last = i === crumbs.length - 1;
          // On a narrow bar only the last two crumbs show; when space runs out, the earlier give
          // way first.
          const early = i < crumbs.length - 2;
          return (
            <li
              key={`${i}-${crumb.label}`}
              className={cx(
                'min-w-0 items-center gap-1.5',
                early ? 'hidden @4xl:flex' : 'flex',
                last ? 'shrink' : 'max-w-48 shrink-[8]',
              )}
            >
              {i > 0 && (
                <span aria-hidden className={cx('text-line-2', i === crumbs.length - 2 && '@max-4xl:hidden')}>
                  /
                </span>
              )}
              {last || crumb.to === undefined ? (
                <span aria-current={last ? 'page' : undefined} className={cx('truncate', last ? 'font-medium text-ink' : 'text-ink-2')}>
                  {crumb.label}
                </span>
              ) : (
                <Link to={crumb.to} activeOptions={{ exact: true }} className="truncate text-ink-2 hover:text-ink hover:underline">
                  {crumb.label}
                </Link>
              )}
            </li>
          );
        })}
      </ol>
    </nav>
  );
}

function StreamState() {
  const { status, problem } = useConnection();
  let pill = null;
  if (problem === 'unauthorized') pill = <StatusPill tone="risk">Token rejected</StatusPill>;
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
            <span className="sr-only @3xl:not-sr-only">Search</span>
            <span className="hidden @4xl:inline-flex">
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
            <span className="sr-only @3xl:not-sr-only">Orchestrator</span>
            <span className="hidden @4xl:inline-flex">
              <Kbd keys={SHORTCUTS.orchestrator} />
            </span>
          </Button>
        </Tooltip>
      </div>
    </header>
  );
}
