// The project page: a header and tabs. The route is
// `projects/$project`; which tab is on is kept in this component's own state, not the URL.

import { useParams } from '@tanstack/react-router';
import { ToggleGroup } from 'radix-ui';
import { useState } from 'react';
import { StatusPill } from '../design/index.ts';
import { ActivityFeed } from './activity.tsx';
import { Board } from './board.tsx';
import { useProjects } from './data.ts';
import { PROJECT_STATUS, formatDay } from './format.ts';
import { ProjectOverviewBody, WorkstreamsTable } from './overview.tsx';
import { ErrorNote } from './ui.tsx';
import { Timeline } from './timeline.tsx';
import { ReadScope } from './read-scope.tsx';

type Tab = 'overview' | 'workstreams' | 'board' | 'timeline' | 'activity';

const TABS: { id: Tab; label: string }[] = [
  { id: 'overview', label: 'Overview' },
  { id: 'workstreams', label: 'Workstreams' },
  { id: 'board', label: 'Board' },
  { id: 'timeline', label: 'Timeline' },
  { id: 'activity', label: 'Activity' },
];

const TAB_ITEM =
  'h-7 rounded-sm px-3 text-sm text-ink-2 data-[state=on]:bg-card data-[state=on]:font-medium data-[state=on]:text-ink data-[state=on]:shadow-sm';

function NotFound() {
  return (
    <div className="mx-auto max-w-3xl px-6 py-6">
      <h1 className="text-xl font-semibold">Project not found</h1>
      <p className="mt-1 text-sm text-ink-2">Nothing lives at this address. It may have moved, or the link is wrong.</p>
    </div>
  );
}

export function ProjectPage() {
  const { project: id }: { project?: string } = useParams({ strict: false });
  const [tab, setTab] = useState<Tab>('overview');
  const projects = useProjects();
  const project = projects.data?.find((p) => p.id === id);

  if (projects.error !== null && project === undefined) {
    return (
      <div className="mx-auto max-w-3xl px-6 py-6">
        <ErrorNote error={projects.error} what="load the project" />
      </div>
    );
  }
  if (projects.data !== undefined && project === undefined) return <NotFound />;

  return (
    <div className="mx-auto flex min-w-0 w-full max-w-5xl flex-col gap-4 px-6 py-6">
      {project !== undefined && <ReadScope key={project.id} scope={`project:${project.id}`} />}
      <header className="flex flex-col gap-2">
        <p className="font-mono text-xs text-ink-2">{project?.key ?? ''}</p>
        <div className="flex flex-wrap items-center gap-2">
          <h1 className="text-2xl font-semibold">{project?.name ?? 'Loading…'}</h1>
          {project !== undefined && (
            <StatusPill tone={PROJECT_STATUS[project.status].tone}>{PROJECT_STATUS[project.status].label}</StatusPill>
          )}
          {project?.due !== undefined && <span className="text-sm text-ink-2">Due {formatDay(project.due)}</span>}
        </div>
        <ToggleGroup.Root
          type="single"
          value={tab}
          onValueChange={(value) => value !== '' && setTab(value as Tab)}
          aria-label="Project sections"
          className="inline-flex w-fit max-w-full flex-wrap rounded-sm border border-line bg-sunken p-0.5"
        >
          {TABS.map((t) => (
            <ToggleGroup.Item key={t.id} value={t.id} className={TAB_ITEM}>
              {t.label}
            </ToggleGroup.Item>
          ))}
        </ToggleGroup.Root>
      </header>

      {project !== undefined && (
        <>
          {tab === 'overview' && <ProjectOverviewBody project={project.id} />}
          {tab === 'workstreams' && <WorkstreamsTable project={project.id} />}
          {tab === 'board' && <Board project={project.id} />}
          {tab === 'timeline' && <Timeline project={project.id} />}
          {tab === 'activity' && <ActivityFeed filters={{ project: project.id }} title="Activity" />}
        </>
      )}
    </div>
  );
}
