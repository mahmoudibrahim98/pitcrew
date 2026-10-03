// Recaps (API v1, "Recaps"): `GET /v1/recaps/blocks` and `GET /v1/recaps/days`.
//
// The mock computes no recaps. It serves crates/fixtures/data/demo-recaps.json, which the recap
// engine writes for the demo workspace's 15 events (`cargo test -p pitcrew-fixtures --test recaps`
// fails when it is stale). So changes made through the mock leave its recaps as they are, and it
// has days for `tz=0` only: any other `tz` is a 400.

import { readFileSync } from 'node:fs';
import { isWellFormedDate } from './rules.ts';
import type { BlocksPage, DayRecap, DaysPage, DemoRecaps } from './types.ts';
import { invalid, isRecord, queryId, queryLimit, queryValue } from './validate.ts';

/** `BLOCKS_DEFAULT_LIMIT`, `BLOCKS_MAX_LIMIT`, … in crates/protocol/src/recap.rs. */
export const BLOCKS_DEFAULT_LIMIT = 50;
export const BLOCKS_MAX_LIMIT = 200;
export const DAYS_DEFAULT_LIMIT = 7;
export const DAYS_MAX_LIMIT = 30;
export const MAX_TZ_MINUTES = 14 * 60;

/** Reads the recaps fixture. */
export function loadRecaps(path: URL): DemoRecaps {
  const parsed: unknown = JSON.parse(readFileSync(path, 'utf8'));
  if (
    !isRecord(parsed) ||
    typeof parsed['tz'] !== 'number' ||
    !Array.isArray(parsed['blocks']) ||
    !Array.isArray(parsed['projects'])
  ) {
    throw new Error(`${path.pathname}: expected an object with tz, blocks and projects`);
  }
  return parsed as unknown as DemoRecaps;
}

/** `tz`: whole minutes east of UTC, within ±14 hours; 0 when absent. */
function queryTz(query: URLSearchParams): number {
  const raw = queryValue(query, 'tz');
  if (raw === undefined) {
    return 0;
  }
  const tz = /^-?\d{1,4}$/.test(raw) ? Number(raw) : Number.NaN;
  if (!(Math.abs(tz) <= MAX_TZ_MINUTES)) {
    throw invalid(`tz must be whole minutes east of UTC, from -${MAX_TZ_MINUTES} to ${MAX_TZ_MINUTES}.`);
  }
  return tz;
}

/** A `YYYY-MM-DD` date in the query; `undefined` when absent. */
function queryDate(query: URLSearchParams, key: string): string | undefined {
  const value = queryValue(query, key);
  if (value !== undefined && !isWellFormedDate(value)) {
    throw invalid(`${key} must be a date written YYYY-MM-DD.`);
  }
  return value;
}

/** Newest first. Ids and dates both sort as text: ULIDs are fixed-width Crockford base32. */
const descending = (a: string, b: string): number => (a < b ? 1 : a > b ? -1 : 0);

/**
 * Blocks, newest first by id, each with its line. Filters combine and match the block's links:
 * `session` its session, `task` one of its tasks, `workstream` and `project` its own. `before` is
 * an exclusive block id; any well-formed id works.
 */
export function blocksPage(recaps: DemoRecaps, query: URLSearchParams): BlocksPage {
  const session = queryId(query, 'session', 'ses');
  const task = queryId(query, 'task', 'tsk');
  const workstream = queryId(query, 'workstream', 'wst');
  const project = queryId(query, 'project', 'prj');
  const before = queryId(query, 'before', 'evt');
  const limit = queryLimit(query, BLOCKS_DEFAULT_LIMIT, BLOCKS_MAX_LIMIT);
  const older = recaps.blocks
    .filter(
      ({ block }) =>
        (session === undefined || block.session === session) &&
        (task === undefined || block.tasks.includes(task)) &&
        (workstream === undefined || block.workstream === workstream) &&
        (project === undefined || block.project === project) &&
        (before === undefined || block.id < before),
    )
    .sort((a, b) => descending(a.block.id, b.block.id));
  return { blocks: older.slice(0, limit), at_start: older.length <= limit };
}

/**
 * Day paragraphs of one workstream or one project, newest date first, whole dates per page.
 * Within a date they keep the engine's order: the one without a workstream first, then by
 * workstream id. `limit` counts dates; `before` is an exclusive date.
 */
export function daysPage(recaps: DemoRecaps, query: URLSearchParams): DaysPage {
  const workstream = queryId(query, 'workstream', 'wst');
  const project = queryId(query, 'project', 'prj');
  if ((workstream === undefined) === (project === undefined)) {
    throw invalid('Give exactly one of workstream and project.');
  }
  const tz = queryTz(query);
  if (tz !== recaps.tz) {
    throw invalid(`The mock hub has recaps for tz=${recaps.tz} only, not tz=${tz}.`);
  }
  const before = queryDate(query, 'before');
  const limit = queryLimit(query, DAYS_DEFAULT_LIMIT, DAYS_MAX_LIMIT);
  // A workstream's entries are the ones it has among its project's days.
  const entries: DayRecap[] =
    project === undefined
      ? recaps.projects.flatMap((p) => p.days.filter((d) => d.workstream === workstream))
      : (recaps.projects.find((p) => p.project === project)?.days ?? []);
  // A stable sort, so entries of one date keep the engine's order.
  const older = entries
    .filter((d) => before === undefined || d.date < before)
    .sort((a, b) => descending(a.date, b.date));
  const dates = [...new Set(older.map((d) => d.date))];
  const kept = new Set(dates.slice(0, limit));
  return { days: older.filter((d) => kept.has(d.date)), at_start: dates.length <= limit };
}
