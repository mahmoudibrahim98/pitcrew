// Recaps in the Projects layout (API v1, "Recaps"; the hooks are the data layer's `recaps.ts`):
// - `RecapSummary`: the Activity "Summary" of a project or workstream, a paragraph per day, newest
//   first, each with a disclosure listing the day's blocks of work;
// - `WorkBlocks`: the blocks of work of a session or task, newest first, with their counts.
// Every clause is marked and leads to its evidence (`recap-text.tsx`).

import { useEffect, useId, useState, type ReactNode } from 'react';
import { Button, ChevronRightIcon } from '../design/index.ts';
import {
  useRecapBlocks,
  useRecapDays,
  type DayRecap,
  type EventId,
  type RecapBlock,
  type RecapBlockFilters,
  type RecapDayScope,
} from '../data/index.ts';
import { cx } from '../lib/cx.ts';
import { useNames } from './data.ts';
import { describeCounts, formatLongDay, formatSpan } from './format.ts';
import { useProjectsNav } from './nav.tsx';
import { SummaryText } from './recap-text.tsx';
import { useRecapTz } from './recap-tz.tsx';
import { ErrorNote, MaybeLink } from './ui.tsx';

const bursts = (n: number): string => `${n} ${n === 1 ? 'burst' : 'bursts'} of work`;

function isDefined<T>(value: T | undefined): value is T {
  return value !== undefined;
}

/** Entries grouped by date, newest date first; within a date, the work outside any workstream first. */
function byDate(days: readonly DayRecap[]): { date: string; entries: DayRecap[] }[] {
  const groups = new Map<string, DayRecap[]>();
  for (const day of days) {
    const entries = groups.get(day.date);
    if (entries === undefined) groups.set(day.date, [day]);
    else entries.push(day);
  }
  const outsideFirst = (d: DayRecap) => (d.workstream === undefined ? 0 : 1);
  return [...groups].map(([date, entries]) => ({
    date,
    entries: entries.toSorted((a, b) => outsideFirst(a) - outsideFirst(b)),
  }));
}

export interface RecapSummaryProps {
  scope: RecapDayScope;
  /** Dates per page (the API's 7 by default). Mainly for tests. */
  daysPerPage?: number;
  /** Blocks per page (the API's 50 by default). Mainly for tests. */
  blocksPerPage?: number;
}

/**
 * Day paragraphs for a project (one per workstream per day, the work outside any workstream first)
 * or a workstream, newest first, paged back to the start with "Load older days".
 */
export function RecapSummary({ scope, daysPerPage, blocksPerPage }: RecapSummaryProps) {
  const tz = useRecapTz();
  const days = useRecapDays(scope, {
    ...(tz === undefined ? {} : { tz }),
    ...(daysPerPage === undefined ? {} : { limit: daysPerPage }),
  });
  const filters: RecapBlockFilters = 'project' in scope ? { project: scope.project } : { workstream: scope.workstream };
  const blocks = useRecapBlocks(filters, blocksPerPage === undefined ? {} : { limit: blocksPerPage });
  const names = useNames();
  const nav = useProjectsNav();
  const openWorkstream = nav.openWorkstream;

  // Blocks load lazily, newest first, separately from the days: when a day's disclosure or a
  // clause's evidence needs blocks older than those loaded, load more until they are.
  const [needed, setNeeded] = useState<EventId | null>(null);
  const oldest = blocks.blocks.at(-1)?.block.id;
  const { hasNextPage, isFetchingNextPage, fetchNextPage } = blocks;
  useEffect(() => {
    if (needed === null || !hasNextPage || isFetchingNextPage) return;
    if (oldest !== undefined && oldest <= needed) return;
    void fetchNextPage();
  }, [needed, oldest, hasNextPage, isFetchingNextPage, fetchNextPage]);
  const byId = new Map(blocks.blocks.map((b) => [b.block.id, b]));
  const need = (ids: readonly EventId[]) => {
    const first = ids.toSorted()[0];
    if (first === undefined || ids.every((id) => byId.has(id))) return;
    setNeeded((prev) => (prev === null || first < prev ? first : prev));
  };
  const blocksState: BlocksState = {
    byId,
    loading:
      blocks.isPending ||
      isFetchingNextPage ||
      (needed !== null && hasNextPage && (oldest === undefined || oldest > needed)),
    error: blocks.error,
    need,
  };

  const project = 'project' in scope;
  const groups = byDate(days.days);
  return (
    <div className="flex flex-col gap-4">
      {days.error !== null && <ErrorNote error={days.error} what="load the summary" />}
      {days.isPending && days.error === null && <p className="text-sm text-ink-2">Loading…</p>}
      {days.data !== undefined && days.days.length === 0 && (
        <p className="text-sm text-ink-2">Nothing has happened here yet.</p>
      )}
      {groups.length > 0 && (
        <ol aria-label="Days" className="flex flex-col gap-6">
          {groups.map(({ date, entries }) => {
            const day = formatLongDay(date);
            return (
              <li key={date} className="flex flex-col gap-3">
                <h3 className="text-sm font-semibold text-ink-2">
                  <time dateTime={date}>{day}</time>
                </h3>
                {project ? (
                  <ul aria-label={`Workstreams, ${day}`} className="flex flex-col gap-4">
                    {entries.map((entry) => {
                      const workstream = entry.workstream;
                      const name = workstream === undefined ? 'Outside any workstream' : names.workstream(workstream);
                      return (
                        <li key={workstream ?? ''} className="flex flex-col gap-1.5">
                          <h4 className="text-md font-semibold">
                            <MaybeLink
                              onOpen={
                                workstream === undefined || openWorkstream === undefined
                                  ? undefined
                                  : () => openWorkstream(workstream)
                              }
                            >
                              {name}
                            </MaybeLink>
                          </h4>
                          <DayEntry entry={entry} label={`${name}, ${day}`} blocks={blocksState} />
                        </li>
                      );
                    })}
                  </ul>
                ) : (
                  entries.map((entry) => (
                    <DayEntry key={entry.workstream ?? ''} entry={entry} label={day} blocks={blocksState} />
                  ))
                )}
              </li>
            );
          })}
        </ol>
      )}
      {days.data !== undefined &&
        (days.atStart ? (
          days.days.length > 0 && <p className="text-xs text-ink-2">That’s everything, back to the start.</p>
        ) : (
          <div>
            <Button variant="ghost" onClick={days.loadMore} disabled={days.isFetchingNextPage}>
              {days.isFetchingNextPage ? 'Loading older days…' : 'Load older days'}
            </Button>
          </div>
        ))}
    </div>
  );
}

interface BlocksState {
  byId: ReadonlyMap<EventId, RecapBlock>;
  loading: boolean;
  error: Error | null;
  /** Asks for these blocks to be loaded (older pages), when they are not yet. */
  need(ids: readonly EventId[]): void;
}

/** One paragraph, and a disclosure listing the blocks of work it covers. */
function DayEntry({ entry, label, blocks }: { entry: DayRecap; label: string; blocks: BlocksState }) {
  const [open, setOpen] = useState(false);
  const listId = useId();
  const covered = entry.blocks.map((id) => blocks.byId.get(id)).filter(isDefined);
  const missing = covered.length < entry.blocks.length;
  return (
    <div className="flex flex-col gap-1.5">
      <SummaryText
        summary={entry.summary}
        blocks={covered}
        onEvidence={() => blocks.need(entry.blocks)}
        className="text-md leading-relaxed"
      />
      <div>
        <button
          type="button"
          aria-expanded={open}
          aria-controls={open ? listId : undefined}
          onClick={() => {
            setOpen(!open);
            if (!open) blocks.need(entry.blocks);
          }}
          className="inline-flex items-center gap-1 rounded-sm text-xs text-ink-2 hover:text-ink"
        >
          <ChevronRightIcon className={cx('size-3 transition-transform', open && 'rotate-90')} />
          {bursts(entry.blocks.length)}
        </button>
        {open && (
          <div id={listId} className="mt-2 border-l border-line pl-3">
            {covered.length > 0 && (
              <ol aria-label={`Bursts of work, ${label}`} className="flex flex-col gap-3">
                {covered.map((recap) => (
                  <BlockRow key={recap.block.id} recap={recap} />
                ))}
              </ol>
            )}
            {missing && blocks.error !== null && <ErrorNote error={blocks.error} what="load the bursts of work" />}
            {missing && blocks.error === null && blocks.loading && <p className="text-sm text-ink-2">Loading…</p>}
          </div>
        )}
      </div>
    </div>
  );
}

/** Where a block sits in the page around it: the session page leaves out its own session, the task page its own task. */
type BlockContext = { session?: string; task?: string };

/** A block of work: when, its line (clauses marked), where it happened, and its counts. */
function BlockRow({ recap, context = {} }: { recap: RecapBlock; context?: BlockContext }) {
  const { block, line } = recap;
  const names = useNames();
  const nav = useProjectsNav();
  const openSession = nav.openSession;
  const openTask = nav.openTask;
  const session = block.session !== undefined && block.session !== context.session ? block.session : undefined;
  const tasks = block.tasks.filter((t) => t !== context.task);
  const where: ReactNode[] = [];
  if (session !== undefined) {
    where.push(
      <span key="session">
        Session{' '}
        <MaybeLink onOpen={openSession === undefined ? undefined : () => openSession(session, 'chat')} className="text-ink">
          {names.session(session)}
        </MaybeLink>
      </span>,
    );
  }
  for (const task of tasks) {
    where.push(
      <MaybeLink key={task} onOpen={openTask === undefined ? undefined : () => openTask(task)} className="font-mono text-ink">
        {names.task(task)}
      </MaybeLink>,
    );
  }
  return (
    <li className="flex flex-col gap-0.5" data-block={block.id}>
      <p className="text-xs text-ink-2">
        <time dateTime={new Date(block.start).toISOString()}>{formatSpan(block.start, block.end)}</time>
      </p>
      <SummaryText summary={line} blocks={[recap]} className="text-sm leading-relaxed" />
      <p className="flex flex-wrap gap-x-3 gap-y-0.5 text-xs text-ink-2">
        {where}
        <span>{describeCounts(block).join(', ')}</span>
      </p>
    </li>
  );
}

export interface WorkBlocksProps {
  /** A session or a task. */
  filters: { session: string } | { task: string };
  /** Blocks per page (the API's 50 by default). Mainly for tests. */
  blocksPerPage?: number;
}

/** The blocks of work of a session or task, newest first, with "Load older". No heading. */
export function WorkBlocks({ filters, blocksPerPage }: WorkBlocksProps) {
  const recap = useRecapBlocks(filters, blocksPerPage === undefined ? {} : { limit: blocksPerPage });
  const context: BlockContext = 'session' in filters ? { session: filters.session } : { task: filters.task };
  return (
    <div className="flex flex-col gap-3">
      {recap.error !== null && <ErrorNote error={recap.error} what="load the bursts of work" />}
      {recap.isPending && recap.error === null && <p className="text-sm text-ink-2">Loading…</p>}
      {recap.data !== undefined && recap.blocks.length === 0 && <p className="text-sm text-ink-2">No work recorded yet.</p>}
      {recap.blocks.length > 0 && (
        <ol aria-label="Bursts of work" className="flex flex-col gap-3">
          {recap.blocks.map((b) => (
            <BlockRow key={b.block.id} recap={b} context={context} />
          ))}
        </ol>
      )}
      {recap.data !== undefined && !recap.atStart && (
        <div>
          <Button variant="ghost" onClick={recap.loadMore} disabled={recap.isFetchingNextPage}>
            {recap.isFetchingNextPage ? 'Loading older…' : 'Load older'}
          </Button>
        </div>
      )}
    </div>
  );
}

/**
 * A session's blocks of work under a "Work" heading, for the console's session page (stream M)
 * to place: `level` is the heading's level there.
 */
export function SessionWork({ session, level = 2 }: { session: string; level?: 2 | 3 }) {
  const id = useId();
  const Heading = level === 2 ? 'h2' : 'h3';
  return (
    <section aria-labelledby={id} className="flex flex-col gap-2">
      <Heading id={id} className="text-md font-semibold">
        Work
      </Heading>
      <WorkBlocks filters={{ session }} />
    </section>
  );
}
