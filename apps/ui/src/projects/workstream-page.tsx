// The workstream page: a header and six tabs.
// The route is `projects/$project/workstreams/$workstream`; the tab on screen is this component's
// own state, not the URL.

import { useBlocker, useParams } from '@tanstack/react-router';
import { ToggleGroup } from 'radix-ui';
import { useState } from 'react';
import { ReadScope } from './read-scope.tsx';
import { StatusPill } from '../design/index.ts';
import { ActivityFeed } from './activity.tsx';
import { DraftBoardPanel, useWaitingProposal } from './board-draft.tsx';
import { Button } from '../design/index.ts';
import { AgentsNow } from './agents.tsx';
import { Board } from './board.tsx';
import { useNames, useWorkstreams } from './data.ts';
import { HEALTH, WORKSTREAM_STATUS } from './format.ts';
import { useProjectsNav } from './nav.tsx';
import { WorkstreamOverviewBody } from './overview.tsx';
import { TasksList } from './tasks-list.tsx';
import { FilesTab } from './files.tsx';
import { ErrorNote, MaybeLink } from './ui.tsx';

type Tab = 'stands' | 'board' | 'tasks' | 'agents' | 'activity' | 'files';

const TABS: { id: Tab; label: string }[] = [
  { id: 'stands', label: 'Where it stands' },
  { id: 'board', label: 'Board' },
  { id: 'tasks', label: 'Tasks' },
  { id: 'agents', label: 'Agents' },
  { id: 'activity', label: 'Activity' },
  { id: 'files', label: 'Files' },
];

const TAB_ITEM =
  'h-7 rounded-sm px-3 text-sm text-ink-2 data-[state=on]:bg-card data-[state=on]:font-medium data-[state=on]:text-ink data-[state=on]:shadow-sm';

function NotFound() {
  return (
    <div className="mx-auto max-w-3xl px-6 py-6">
      <h1 className="text-xl font-semibold">Workstream not found</h1>
      <p className="mt-1 text-sm text-ink-2">Nothing lives at this address. It may have moved, or the link is wrong.</p>
    </div>
  );
}

export function WorkstreamPage() {
  const { workstream: id }: { workstream?: string } = useParams({ strict: false });
  const [tab, setTab] = useState<Tab>('stands');
  const [drafting, setDrafting] = useState(false);
  const [filesDirty, setFilesDirty] = useState(false);
  const [filesBusy, setFilesBusy] = useState(false);
  useBlocker({
    disabled: !filesDirty && !filesBusy,
    shouldBlockFn: () => filesBusy || (filesDirty && !window.confirm('Discard unsaved changes?')),
    enableBeforeUnload: false,
  });
  const workstreams = useWorkstreams();
  const names = useNames();
  const nav = useProjectsNav();
  const workstream = workstreams.data?.find((w) => w.id === id);
  const openProject = nav.openProject;

  if (workstreams.error !== null && workstream === undefined) {
    return (
      <div className="mx-auto max-w-3xl px-6 py-6">
        <ErrorNote error={workstreams.error} what="load the workstream" />
      </div>
    );
  }
  if (workstreams.data !== undefined && workstream === undefined) return <NotFound />;

  return (
    <div className="mx-auto flex max-w-5xl flex-col gap-4 px-6 py-6">
      {workstream !== undefined && <ReadScope key={workstream.id} scope={`workstream:${workstream.id}`} />}
      <header className="flex flex-col gap-2">
        {workstream !== undefined && (
          <MaybeLink
            onOpen={openProject === undefined ? undefined : () => openProject(workstream.project)}
            className="text-xs text-ink-2"
          >
            {names.project(workstream.project)}
          </MaybeLink>
        )}
        <div className="flex flex-wrap items-center gap-2">
          <h1 className="text-2xl font-semibold">{workstream?.name ?? 'Loading…'}</h1>
          {workstream !== undefined && (
            <>
              <StatusPill tone={WORKSTREAM_STATUS[workstream.status].tone}>
                {WORKSTREAM_STATUS[workstream.status].label}
              </StatusPill>
              <StatusPill tone={HEALTH[workstream.health].tone}>{HEALTH[workstream.health].label}</StatusPill>
              <Button className="ml-auto" aria-expanded={drafting} onClick={() => setDrafting((d) => !d)}>
                Draft board
              </Button>
            </>
          )}
        </div>
        {workstream !== undefined && !drafting && (
          <WaitingProposal workstream={workstream.id} onReview={() => setDrafting(true)} />
        )}
        <ToggleGroup.Root
          type="single"
          value={tab}
          onValueChange={(value) => {
            if (value === '' || value === tab || filesBusy) return;
            if (filesDirty && !window.confirm('Discard unsaved changes?')) return;
            setFilesDirty(false);
            setTab(value as Tab);
          }}
          aria-label="Workstream sections"
          className="inline-flex w-fit flex-wrap rounded-sm border border-line bg-sunken p-0.5"
        >
          {TABS.map((t) => (
            <ToggleGroup.Item key={t.id} value={t.id} className={TAB_ITEM}>
              {t.label}
            </ToggleGroup.Item>
          ))}
        </ToggleGroup.Root>
      </header>

      {workstream !== undefined && drafting && (
        <DraftBoardPanel workstream={workstream.id} onClose={() => setDrafting(false)} />
      )}
      {workstream !== undefined && (
        <>
          {tab === 'stands' && <WorkstreamOverviewBody workstream={workstream.id} />}
          {tab === 'board' && <Board workstream={workstream.id} />}
          {tab === 'tasks' && <TasksList workstream={workstream.id} />}
          {tab === 'agents' && <AgentsNow workstream={workstream.id} title="Agents" />}
          {tab === 'activity' && <ActivityFeed filters={{ workstream: workstream.id }} title="Activity" />}
          {tab === 'files' && <FilesTab key={workstream.id} workstream={workstream} onDirty={setFilesDirty} onBusy={setFilesBusy} />}
        </>
      )}
    </div>
  );
}

/** A notice when an agent's drafted board waits for this person's review. */
function WaitingProposal({ workstream, onReview }: { workstream: string; onReview: () => void }) {
  const waiting = useWaitingProposal(workstream);
  if (waiting === undefined) return null;
  const count = waiting.proposal?.tasks.length ?? 0;
  return (
    <p role="status" className="flex flex-wrap items-center gap-2 rounded-sm border border-line bg-accent-soft px-3 py-2 text-sm text-ink">
      A drafted board of {count} task{count === 1 ? '' : 's'} is waiting for your review. Nothing is created until you
      accept it.
      <Button variant="secondary" onClick={onReview}>
        Review the draft
      </Button>
    </p>
  );
}
