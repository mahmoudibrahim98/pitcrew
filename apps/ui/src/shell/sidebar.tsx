// The sidebar: the workspace switcher, the entries for the layout on screen (the shell's and the
// features'), the Projects tree in the Projects layout, and the features' sidebar panels. It
// collapses to a rail of icons (Ctrl B).

import { Link, useRouter } from '@tanstack/react-router';
import { Suspense, useId, type ReactNode } from 'react';
import { useMe, useWorkspace } from '../data/index.ts';
import {
  Avatar,
  ChevronsUpDownIcon,
  Menu,
  MenuContent,
  MenuItem,
  MenuLabel,
  MenuRadioGroup,
  MenuRadioItem,
  MenuSeparator,
  MenuTrigger,
  SidebarIcon,
  ThemeToggle,
  Tooltip,
} from '../design/index.ts';
import { cx } from '../lib/cx.ts';
import { useRegistry } from './context.ts';
import { inLayout } from './feature.ts';
import { useLayout, useWorkspaceId } from './layout.ts';
import { paths } from './paths.ts';
import { ProjectsTree } from './projects-tree.tsx';
import type { ResolvedNav } from './registry.ts';
import { SHORTCUTS } from './shortcuts.ts';
import { useShell } from './store.ts';

const ITEM =
  'flex h-7 min-w-0 items-center gap-2 rounded-sm px-2 text-sm text-ink-2 outline-none ' +
  'hover:bg-hover hover:text-ink focus-visible:outline-2 focus-visible:outline-accent ' +
  'aria-[current=page]:bg-card aria-[current=page]:font-medium aria-[current=page]:text-ink ' +
  'aria-[current=page]:shadow-[0_0_0_1px_var(--pc-line)]';

function WorkspaceSwitcher({ collapsed }: { collapsed: boolean }) {
  const router = useRouter();
  const ws = useWorkspaceId();
  const workspace = useWorkspace().data?.workspace;
  // One workspace for now; the menu lists however many the hub reports.
  const workspaces = workspace === undefined ? [] : [workspace];
  const name = workspace?.name ?? 'Workspace';
  return (
    <Menu>
      <MenuTrigger asChild>
        <button
          type="button"
          aria-label={`Workspace: ${name}`}
          className="flex h-9 min-w-0 flex-1 items-center gap-2 rounded-md px-1.5 text-left outline-none hover:bg-hover focus-visible:outline-2 focus-visible:outline-accent"
        >
          <span
            aria-hidden
            className="inline-flex size-6 shrink-0 items-center justify-center rounded-sm bg-ink text-xs font-semibold text-bg"
          >
            {name.slice(0, 1).toUpperCase()}
          </span>
          {!collapsed && (
            <>
              <span className="min-w-0 flex-1 truncate text-sm font-semibold text-ink">{name}</span>
              <ChevronsUpDownIcon className="size-3.5 text-ink-2" />
            </>
          )}
        </button>
      </MenuTrigger>
      <MenuContent className="w-60">
        <MenuLabel>Workspaces</MenuLabel>
        <MenuRadioGroup
          value={ws}
          onValueChange={(id) => void router.navigate({ href: paths.workspace(id) })}
        >
          {workspaces.map((w) => (
            <MenuRadioItem key={w.id} value={w.id}>
              {w.name}
            </MenuRadioItem>
          ))}
        </MenuRadioGroup>
        <MenuSeparator />
        <MenuItem disabled>Add a workspace (coming later)</MenuItem>
      </MenuContent>
    </Menu>
  );
}

function NavLink({ entry, collapsed }: { entry: ResolvedNav; collapsed: boolean }) {
  const ws = useWorkspaceId();
  const Icon = entry.icon;
  const Count = entry.badge;
  const link = (
    <Link
      to={paths.under(ws, entry.to)}
      aria-label={collapsed ? entry.label : undefined}
      data-nav={entry.id}
      className={cx(ITEM, collapsed && 'justify-center px-0')}
    >
      {Icon === undefined ? (
        <span aria-hidden className="inline-flex size-4 shrink-0 items-center justify-center text-xs font-semibold">
          {entry.label.slice(0, 1)}
        </span>
      ) : (
        <Icon className="size-4 shrink-0" />
      )}
      {!collapsed && <span className="min-w-0 flex-1 truncate">{entry.label}</span>}
      {!collapsed && Count !== undefined && <Count />}
    </Link>
  );
  return collapsed ? (
    <Tooltip content={entry.label} side="right">
      {link}
    </Tooltip>
  ) : (
    link
  );
}

function Section({ label, children }: { label: string; children: ReactNode }) {
  const id = useId();
  return (
    <nav aria-labelledby={id} className="flex flex-col gap-px">
      <h2 id={id} className="px-2 pt-3 pb-1 text-xs font-medium text-ink-2">
        {label}
      </h2>
      {children}
    </nav>
  );
}

function Footer({ collapsed }: { collapsed: boolean }) {
  const me = useMe().data;
  return (
    <div className="flex flex-col gap-2 border-t border-line p-2">
      {me !== undefined && (
        <div className={cx('flex items-center gap-2 px-1', collapsed && 'justify-center px-0')}>
          <Avatar member={me} size="sm" />
          {!collapsed && (
            <span className="min-w-0 flex-1 truncate text-sm text-ink" data-testid="me">
              {me.name}
            </span>
          )}
        </div>
      )}
      {!collapsed && <ThemeToggle />}
    </div>
  );
}

export function Sidebar() {
  const registry = useRegistry();
  const layout = useLayout();
  const collapsed = useShell((s) => s.sidebarCollapsed);
  const setCollapsed = useShell((s) => s.setSidebarCollapsed);

  const entries = registry.nav.filter((e) => inLayout(e.layout, layout));
  const top = entries.filter((e) => e.section === undefined);
  const sections = [...new Set(entries.flatMap((e) => (e.section === undefined ? [] : [e.section])))];
  const panels = registry.sidebars.filter((p) => inLayout(p.layout, layout));

  const toggle = (
    <Tooltip content={collapsed ? 'Expand sidebar' : 'Collapse sidebar'} keys={SHORTCUTS.sidebar} side="right">
      <button
        type="button"
        aria-label={collapsed ? 'Expand sidebar' : 'Collapse sidebar'}
        onClick={() => setCollapsed(!collapsed)}
        className="inline-flex size-7 shrink-0 items-center justify-center rounded-sm text-ink-2 outline-none hover:bg-hover hover:text-ink focus-visible:outline-2 focus-visible:outline-accent"
      >
        <SidebarIcon />
      </button>
    </Tooltip>
  );

  return (
    <aside
      aria-label="Sidebar"
      data-collapsed={collapsed}
      className={cx(
        'flex h-full shrink-0 flex-col border-r border-line bg-sidebar',
        collapsed ? 'w-13' : 'w-(--pc-sidebar-width)',
      )}
    >
      <div className={cx('flex items-center gap-1 p-2', collapsed && 'flex-col')}>
        <WorkspaceSwitcher collapsed={collapsed} />
        {toggle}
      </div>
      <div className="flex min-h-0 flex-1 flex-col gap-1 overflow-y-auto px-2 pb-2">
        <nav aria-label="Main" className="flex flex-col gap-px">
          {top.map((entry) => (
            <NavLink key={entry.id} entry={entry} collapsed={collapsed} />
          ))}
        </nav>
        {!collapsed &&
          sections.map((section) => (
            <Section key={section} label={section}>
              {entries
                .filter((e) => e.section === section)
                .map((entry) => (
                  <NavLink key={entry.id} entry={entry} collapsed={false} />
                ))}
            </Section>
          ))}
        {!collapsed && layout === 'projects' && (
          <Section label="Projects">
            <ProjectsTree />
          </Section>
        )}
        {!collapsed &&
          panels.map(({ feature, component: Panel }) => (
            <Suspense key={feature} fallback={null}>
              <Panel />
            </Suspense>
          ))}
      </div>
      <Footer collapsed={collapsed} />
    </aside>
  );
}
