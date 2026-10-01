// Activity: what happened, newest first. "Summary" is the recap (`recaps.tsx`): a paragraph per
// day for a project or workstream, the blocks of work for a task or session. "All events" is the
// raw feed from `GET /v1/events`.

import { ToggleGroup } from 'radix-ui';
import { useState } from 'react';
import { Button } from '../design/index.ts';
import { ApiError, type Event, type Member, type MemberId } from '../data/index.ts';
import { useActivity, useMemberMap, useNames, type EventFilters } from './data.ts';
import { describeEvent, formatWhen, type Names } from './format.ts';
import { Avatar } from './people.tsx';
import { RecapSummary, WorkBlocks } from './recaps.tsx';
import { ErrorNote, Panel } from './ui.tsx';

export function AuthorAvatar({ event, members }: { event: Event; members: ReadonlyMap<MemberId, Member> }) {
  const author = members.get(event.author);
  if (author === undefined) return <span aria-hidden className="size-5 shrink-0" />;
  const owner = author.owner === undefined ? undefined : members.get(author.owner);
  // The handle is written next to it.
  return <Avatar member={author} owner={owner} decorative />;
}

/** Events, newest first, each as "who did what, when". */
export function EventList({
  events,
  members,
  names,
  label,
}: {
  events: readonly Event[];
  members: ReadonlyMap<MemberId, Member>;
  names: Names;
  label: string;
}) {
  return (
    <ol aria-label={label} className="flex flex-col gap-1.5">
      {events.map((event) => {
        const author = members.get(event.author);
        return (
          <li key={event.id} className="flex items-start gap-2 text-sm">
            <AuthorAvatar event={event} members={members} />
            <p className="min-w-0 flex-1">
              <span className="font-medium">{author?.handle ?? names.member(event.author)}</span>{' '}
              <span className="text-ink-2">{describeEvent(event, names)}</span>
            </p>
            <time dateTime={new Date(event.at).toISOString()} className="shrink-0 text-xs text-ink-2">
              {formatWhen(event.at)}
            </time>
          </li>
        );
      })}
    </ol>
  );
}

type View = 'summary' | 'all';

/** The recap for the same filters: day paragraphs for a workstream or project, blocks of work for a task or session. */
function Summary({ filters }: { filters: EventFilters }) {
  if (filters.workstream !== undefined) return <RecapSummary scope={{ workstream: filters.workstream }} />;
  if (filters.project !== undefined) return <RecapSummary scope={{ project: filters.project }} />;
  if (filters.task !== undefined) return <WorkBlocks filters={{ task: filters.task }} />;
  if (filters.session !== undefined) return <WorkBlocks filters={{ session: filters.session }} />;
  return <p className="text-sm text-ink-2">Summaries are kept per project, workstream, task and session.</p>;
}

const TOGGLE_ITEM =
  'h-6 rounded-sm px-2 text-xs text-ink-2 data-[state=on]:bg-card data-[state=on]:text-ink data-[state=on]:shadow-sm';

export function ActivityFeed({
  filters = {},
  title = 'Recent activity',
  initialView = 'all',
}: {
  filters?: EventFilters;
  title?: string;
  initialView?: View;
}) {
  const [view, setView] = useState<View>(initialView);
  const activity = useActivity(filters);
  const members = useMemberMap();
  const names = useNames();
  const events = [...(activity.data?.events ?? [])].reverse();
  // The real hub answers 400 to a `project` or `workstream` filter until its index lands (the
  // mock accepts them); that is not an error to show, just activity this view cannot offer yet.
  const scoped = filters.project !== undefined || filters.workstream !== undefined;
  const unavailable = scoped && activity.error instanceof ApiError && activity.error.code === 'invalid';
  return (
    <Panel
      title={title}
      actions={
        <ToggleGroup.Root
          type="single"
          value={view}
          onValueChange={(value) => value !== '' && setView(value as View)}
          aria-label="Activity view"
          className="inline-flex rounded-sm border border-line bg-sunken p-0.5"
        >
          <ToggleGroup.Item value="summary" className={TOGGLE_ITEM}>
            Summary
          </ToggleGroup.Item>
          <ToggleGroup.Item value="all" className={TOGGLE_ITEM}>
            All events
          </ToggleGroup.Item>
        </ToggleGroup.Root>
      }
    >
      {view === 'summary' ? (
        <Summary filters={filters} />
      ) : unavailable ? (
        <p className="text-sm text-ink-2">Activity isn’t available here yet.</p>
      ) : (
        <div className="flex flex-col gap-2">
          {activity.error !== null && <ErrorNote error={activity.error} what="load the activity" />}
          {activity.isPending && activity.error === null && <p className="text-sm text-ink-2">Loading…</p>}
          {activity.data !== undefined && events.length === 0 && <p className="text-sm text-ink-2">Nothing yet.</p>}
          {events.length > 0 && <EventList events={events} members={members} names={names} label="Events" />}
          {activity.data !== undefined && !activity.data.at_start && (
            <div>
              <Button variant="ghost" onClick={activity.loadOlder} disabled={activity.isFetching}>
                Load older
              </Button>
            </div>
          )}
        </div>
      )}
    </Panel>
  );
}
