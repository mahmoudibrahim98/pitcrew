// A fake `GET /v1/events` feed that behaves as the contract describes it, including what the mock
// hub does not do yet: with filters the hub scans a bounded window per request, so a page may hold
// fewer events than asked for, or none, and still not be at the start.

import type { Event } from '../../data/index.ts';
import type { ActivityPage } from '../data.ts';

export interface FakeFeed {
  /** Revisions 1..total exist. */
  total: number;
  /** Which revisions match the request's filters. */
  matches?: (rev: number) => boolean;
  /** Revisions one request scans at most. */
  scan?: number;
  /** The event at a revision. */
  event: (rev: number) => Event;
}

/**
 * One page for `before` (exclusive) and `limit`. An empty page that is not at the start has
 * `to_rev = 0` and `from_rev` = where its scan stopped. Like a hub with a bounded scan, it knows it
 * is at the start only once a scan reaches revision 1.
 */
export function fakePage(feed: FakeFeed, query: Record<string, string | undefined>): ActivityPage {
  const matches = feed.matches ?? (() => true);
  const scan = feed.scan ?? Infinity;
  const limit = Number(query.limit ?? 100);
  const before = query.before === undefined ? feed.total + 1 : Number(query.before);
  const revs: number[] = [];
  let rev = before - 1;
  for (let scanned = 0; rev >= 1 && revs.length < limit && scanned < scan; rev--, scanned++) {
    if (matches(rev)) revs.push(rev);
  }
  revs.reverse();
  return {
    events: revs.map(feed.event),
    from_rev: revs[0] ?? rev + 1,
    to_rev: revs.at(-1) ?? 0,
    at_start: rev < 1,
  };
}
